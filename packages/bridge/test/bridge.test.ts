import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { request as httpRequest } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import {
  describeRefusal,
  parseHealthResponse,
  type HealthResponse,
} from "@mokmok/web-protocol";

import { parseBridgeConfig, startBridge } from "../src/index.ts";
import type { BridgeHandle, BridgeOptions } from "../src/index.ts";
import { BrowserClient, batchThrough } from "./browser-client.ts";
import { FakeBus, type FakeBusOptions } from "./fake-bus.ts";

const UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

const sleep = (millis: number) =>
  new Promise<void>((resolvePromise) => setTimeout(resolvePromise, millis));

const until = async (
  predicate: () => boolean | Promise<boolean>,
  label: string,
  timeoutMs = 5_000,
): Promise<void> => {
  const deadline = Date.now() + timeoutMs;
  while (!(await predicate())) {
    if (Date.now() >= deadline) {
      throw new Error(`timed out waiting for ${label}`);
    }
    await sleep(10);
  }
};

const clients: BrowserClient[] = [];
const bridges: BridgeHandle[] = [];
const buses: FakeBus[] = [];
const directories: string[] = [];

const tempDir = async (): Promise<string> => {
  const directory = await mkdtemp(join(tmpdir(), "mokmok-web-bridge-"));
  directories.push(directory);

  return directory;
};

const makeBus = async (
  directory: string,
  options: FakeBusOptions = {},
): Promise<FakeBus> => {
  const bus = new FakeBus(join(directory, "bus.sock"), options);
  await bus.start();
  buses.push(bus);

  return bus;
};

const makeBridge = async (options: BridgeOptions): Promise<BridgeHandle> => {
  const bridge = await startBridge(options);
  bridges.push(bridge);

  return bridge;
};

const bridgeFor = (
  directory: string,
  bus: FakeBus,
  overrides: Partial<BridgeOptions> = {},
): Promise<BridgeHandle> =>
  makeBridge({
    socketPath: bus.socketPath,
    topic: "global",
    subscriberFile: join(directory, "subscriber-id"),
    port: 0,
    ...overrides,
  });

const openBrowser = async (port: number): Promise<BrowserClient> => {
  const client = await BrowserClient.open(port);
  clients.push(client);

  return client;
};

const fetchHealth = async (port: number): Promise<HealthResponse> => {
  const response = await fetch(`http://127.0.0.1:${port}/api/health`);
  expect(response.status).toBe(200);
  const text = await response.text();
  const health = parseHealthResponse(text);
  expect(health).not.toBeNull();

  return health as HealthResponse;
};

const rawGet = (
  port: number,
  path: string,
): Promise<{ readonly status: number; readonly body: string }> =>
  new Promise((resolvePromise, reject) => {
    const request = httpRequest(
      { host: "127.0.0.1", port, path, method: "GET" },
      (response) => {
        const chunks: Buffer[] = [];
        response.on("data", (chunk: Buffer) => chunks.push(chunk));
        response.on("end", () => {
          resolvePromise({
            status: response.statusCode ?? 0,
            body: Buffer.concat(chunks).toString("utf8"),
          });
        });
      },
    );
    request.on("error", reject);
    request.end();
  });

afterEach(async () => {
  for (const client of clients.splice(0).reverse()) {
    try {
      await client.close();
    } catch {
      // Already closed by the bridge.
    }
  }
  for (const bridge of bridges.splice(0).reverse()) {
    try {
      await bridge.close();
    } catch {
      // Already closed by the test.
    }
  }
  for (const bus of buses.splice(0).reverse()) {
    try {
      await bus.stop();
    } catch {
      // Already stopped by the test.
    }
  }
  for (const directory of directories.splice(0).reverse()) {
    await rm(directory, { recursive: true, force: true }).catch(() => {});
  }
});

describe("web bridge", () => {
  it("opens the bus with a persisted subscriber id and greets the browser", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory);
    const bridge = await bridgeFor(directory, bus);
    const browser = await openBrowser(bridge.port);

    const hello = await browser.nextFrame("hello");
    expect(hello.topic).toBe("global");
    expect(hello.subscriber).toMatch(UUID_RE);
    expect(hello.subscriber).toBe(bridge.subscriberId);

    await browser.waitStatus("connecting");
    await browser.waitStatus("live");

    expect(bus.connectionCount).toBe(1);
    await until(() => bus.openRequests.length === 1, "one open request");
    const open = bus.openRequests[0];
    expect(open?.topic).toBe("global");
    expect(open?.subscriber).toBe(hello.subscriber);

    await until(() => bus.pullRequests.length >= 1, "a pull request");
    expect(bus.pullRequests[0]?.max).toBeGreaterThan(0);

    const persisted = (await readFile(join(directory, "subscriber-id"), "utf8"))
      .trim();
    expect(persisted).toBe(hello.subscriber);
  });

  it("forwards one batch and only acks the bus after the browser acks", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory);
    const bridge = await bridgeFor(directory, bus);
    const browser = await openBrowser(bridge.port);
    await browser.waitStatus("live");

    const event = bus.emit({ message: "one" });
    const batch = await browser.nextFrame("batch");
    expect(batch.events.map((candidate) => candidate.id)).toEqual([event.id]);
    expect(batch.window.epoch).toBe(bus.epoch);

    await sleep(150);
    expect(bus.ackRequests).toHaveLength(0);

    browser.ack(batch);
    const acked = await browser.nextFrame("acked");
    expect(acked.through).toEqual(batchThrough(batch));

    await until(() => bus.ackRequests.length === 1, "the bus ack");
    expect(bus.ackRequests[0]?.through).toEqual(batchThrough(batch));

    bus.emit({ message: "two" });
    const second = await browser.nextFrame("batch");
    expect(second.events.map((candidate) => candidate.seq)).toEqual([1]);
    expect(bus.pullRequests.length).toBeGreaterThanOrEqual(2);
  });

  it("leaves the bus unacked when the browser disconnects before acking, then replays", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory);
    const bridge = await bridgeFor(directory, bus);

    const event = bus.emit({ message: "replay me" });
    const first = await openBrowser(bridge.port);
    await first.waitStatus("live");
    const firstBatch = await first.nextFrame("batch");
    expect(firstBatch.events.map((candidate) => candidate.id)).toEqual([
      event.id,
    ]);

    await first.close();
    await sleep(150);
    expect(bus.ackRequests).toHaveLength(0);
    expect(bus.checkpoints.get(bridge.subscriberId)).toBe(0);

    const second = await openBrowser(bridge.port);
    const hello = await second.nextFrame("hello");
    expect(hello.subscriber).toBe(bridge.subscriberId);
    await second.waitStatus("live");

    const replayed = await second.nextFrame("batch");
    expect(replayed.events.map((candidate) => candidate.id)).toEqual([event.id]);
    expect(bus.ackRequests).toHaveLength(0);

    second.ack(replayed);
    await second.nextFrame("acked");
    await until(() => bus.ackRequests.length === 1, "the bus ack after replay");
    expect(bus.ackRequests[0]?.through.seq).toBe(1);
    expect(bus.openRequests).toHaveLength(2);
    expect(
      bus.openRequests.every((open) => open.subscriber === bridge.subscriberId),
    ).toBe(true);
  });

  it("reuses the persisted subscriber id across a restart and resumes the checkpoint", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory);
    const options: BridgeOptions = {
      socketPath: bus.socketPath,
      topic: "global",
      subscriberFile: join(directory, "subscriber-id"),
      port: 0,
    };

    const first = await makeBridge(options);
    const firstBrowser = await openBrowser(first.port);
    const firstHello = await firstBrowser.nextFrame("hello");
    await firstBrowser.waitStatus("live");

    bus.emit({ message: "before restart" });
    const batch = await firstBrowser.nextFrame("batch");
    firstBrowser.ack(batch);
    await firstBrowser.nextFrame("acked");
    await until(() => bus.ackRequests.length === 1, "the pre-restart ack");

    await first.close();

    const second = await makeBridge(options);
    expect(second.subscriberId).toBe(firstHello.subscriber);

    const secondBrowser = await openBrowser(second.port);
    const secondHello = await secondBrowser.nextFrame("hello");
    expect(secondHello.subscriber).toBe(firstHello.subscriber);
    await secondBrowser.waitStatus("live");

    const afterRestart = bus.emit({ message: "after restart" });
    const resumed = await secondBrowser.nextFrame("batch");
    expect(resumed.events.map((candidate) => candidate.id)).toEqual([
      afterRestart.id,
    ]);
    expect(bus.openRequests.map((open) => open.subscriber)).toEqual([
      firstHello.subscriber,
      firstHello.subscriber,
    ]);
  });

  it("surfaces a bus refusal as status refused and retries with backoff", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory, { refuseOpen: "topic_limit" });
    const bridge = await bridgeFor(directory, bus);
    const browser = await openBrowser(bridge.port);

    await browser.nextFrame("hello");
    const refused = await browser.waitStatus("refused");
    expect(refused.detail).toBe(describeRefusal("topic_limit"));

    await until(() => bus.openRequests.length >= 2, "a refused reconnect");
    await until(
      async () => (await fetchHealth(bridge.port)).status === "refused",
      "health to report the refusal",
    );
  });

  it("rejects a second browser with a distinct close code", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory);
    const bridge = await bridgeFor(directory, bus);
    const first = await openBrowser(bridge.port);
    await first.waitStatus("live");

    const second = new BrowserClient(bridge.port);
    const closed = await second.waitClosed();

    expect(closed.code).toBe(4009);
    expect(closed.reason.length).toBeGreaterThan(0);
    expect(first.isOpen).toBe(true);
  });

  it("serves a valid health document with live batch state", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory);
    const bridge = await bridgeFor(directory, bus);
    const browser = await openBrowser(bridge.port);
    await browser.waitStatus("live");

    bus.emit({ message: "health" });
    const batch = await browser.nextFrame("batch");

    const health = await fetchHealth(bridge.port);
    expect(health.status).toBe("live");
    expect(health.topic).toBe("global");
    expect(health.subscriber).toBe(bridge.subscriberId);
    expect(health.socket).toBe(bus.socketPath);
    expect(health.events_delivered).toBe(1);
    expect(health.last_seq).toBe(0);
    expect(health.pending_batch).toBe(batch.batch_id);
    expect(health.connected_at_ms).not.toBeNull();

    browser.ack(batch);
    await browser.nextFrame("acked");
    await until(
      async () => (await fetchHealth(bridge.port)).pending_batch === null,
      "health to clear the pending batch",
    );
  });

  it("serves static files, falls back to index.html, and refuses traversal", async () => {
    const directory = await tempDir();
    const publicDir = join(directory, "public");
    const index = "<html><body>mokmok console</body></html>";
    const script = "console.log('bridge');";
    await mkdir(join(publicDir, "assets"), { recursive: true });
    await writeFile(join(publicDir, "index.html"), index);
    await writeFile(join(publicDir, "assets", "app.js"), script);

    const bridge = await makeBridge({
      socketPath: join(directory, "missing-bus.sock"),
      topic: "global",
      subscriberFile: join(directory, "subscriber-id"),
      port: 0,
      staticDir: publicDir,
    });

    const root = await fetch(`http://127.0.0.1:${bridge.port}/`);
    expect(root.status).toBe(200);
    expect(root.headers.get("content-type")).toContain("text/html");
    expect(await root.text()).toBe(index);

    const asset = await fetch(
      `http://127.0.0.1:${bridge.port}/assets/app.js`,
    );
    expect(asset.status).toBe(200);
    expect(asset.headers.get("content-type")).toContain("javascript");
    expect(await asset.text()).toBe(script);

    const spaRoute = await fetch(
      `http://127.0.0.1:${bridge.port}/events/42`,
    );
    expect(spaRoute.status).toBe(200);
    expect(await spaRoute.text()).toBe(index);

    const missing = await fetch(
      `http://127.0.0.1:${bridge.port}/missing.txt`,
    );
    expect(missing.status).toBe(404);

    const traversal = await rawGet(bridge.port, "/..%2f..%2fetc%2fpasswd");
    expect(traversal.status).not.toBe(200);
    expect(traversal.body).not.toContain("root:");

    const encodedTraversal = await rawGet(
      bridge.port,
      "/%2e%2e/%2e%2e/etc/passwd",
    );
    expect(encodedTraversal.status).not.toBe(200);
    expect(encodedTraversal.body).not.toContain("root:");
  });

  it("reports a missing static directory clearly", async () => {
    const directory = await tempDir();
    const missingDirectory = join(directory, "missing");
    const bridge = await makeBridge({
      socketPath: join(directory, "missing-bus.sock"),
      topic: "global",
      subscriberFile: join(directory, "subscriber-id"),
      port: 0,
      staticDir: missingDirectory,
    });

    const response = await fetch(`http://127.0.0.1:${bridge.port}/`);
    expect(response.status).toBe(404);
    expect(await response.text()).toContain("not found");

    const unconfigured = await makeBridge({
      socketPath: join(directory, "missing-bus.sock"),
      topic: "global",
      subscriberFile: join(directory, "subscriber-id"),
      port: 0,
    });
    const unconfiguredResponse = await fetch(
      `http://127.0.0.1:${unconfigured.port}/`,
    );
    expect(unconfiguredResponse.status).toBe(404);
    expect(await unconfiguredResponse.text()).toContain("not configured");
  });

  it("forwards a browser publish and returns the bus receipt", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory);
    const bridge = await bridgeFor(directory, bus);
    const browser = await openBrowser(bridge.port);
    await browser.waitStatus("live");

    browser.publish("req-1", { kind: "ping", n: 1 });

    const published = await browser.nextFrame("published");
    expect(published.request_id).toBe("req-1");
    expect(published.id).toMatch(UUID_RE);
    expect(published.seq).toBe(0);
    expect(published.at.secs_since_epoch).toBeGreaterThan(0);

    await until(() => bus.publishRequests.length === 1, "the bus publish");
    expect(bus.publishRequests[0]?.payload).toEqual({ kind: "ping", n: 1 });

    // The publisher gets its own connection and opens it as a publisher first.
    expect(bus.openRequests.filter((open) => open.subscriber === null)).toHaveLength(1);
    expect(bus.connectionCount).toBe(2);

    const batch = await browser.nextFrame("batch");
    expect(batch.events.map((candidate) => candidate.id)).toEqual([
      published.id,
    ]);
    browser.ack(batch);
    await browser.nextFrame("acked");
  });

  it("replaces an invalid subscriber id file with a fresh persisted uuid", async () => {
    const directory = await tempDir();
    const subscriberFile = join(directory, "subscriber-id");
    await writeFile(subscriberFile, "not-a-uuid\n");

    const bridge = await makeBridge({
      socketPath: join(directory, "missing-bus.sock"),
      topic: "global",
      subscriberFile,
      port: 0,
    });

    expect(bridge.subscriberId).toMatch(UUID_RE);
    expect((await readFile(subscriberFile, "utf8")).trim()).toBe(
      bridge.subscriberId,
    );
    expect((await readdir(directory)).some((name) => name.endsWith(".tmp"))).toBe(
      false,
    );
  });

  it("keeps delivering batches while a publish waits for its receipt", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory, { onPublish: () => "silence" });
    const bridge = await bridgeFor(directory, bus);
    const browser = await openBrowser(bridge.port);
    await browser.waitStatus("live");

    browser.publish("req-slow", { kind: "slow" });
    await until(() => bus.publishRequests.length === 1, "the pending publish");

    const event = bus.emit({ message: "while publishing" });
    const batch = await browser.nextFrame("batch");
    expect(batch.events.map((candidate) => candidate.id)).toEqual([event.id]);
    expect(browser.frames.some((frame) => frame.op === "publish_failed")).toBe(
      false,
    );
  });

  it("surfaces a bus transport failure as status offline", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory);
    const bridge = await bridgeFor(directory, bus);
    const browser = await openBrowser(bridge.port);
    await browser.waitStatus("live");

    await bus.stop();

    const offline = await browser.waitStatus("offline", 8_000);
    expect(offline.detail).toBeTruthy();
    await until(
      async () => (await fetchHealth(bridge.port)).status === "offline",
      "health to report offline",
    );
  });

  it("closes a browser frame larger than the payload bound", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory);
    const bridge = await bridgeFor(directory, bus);
    const browser = await openBrowser(bridge.port);
    await browser.waitStatus("live");

    browser.sendRaw("x".repeat(1024 * 1024 + 1));

    const closed = await browser.waitClosed(8_000);
    expect(closed.code).toBe(1009);
  });

  it("reports an indeterminate publish failure when the write was lost", async () => {    const directory = await tempDir();
    const bus = await makeBus(directory, { onPublish: () => "drop" });
    const bridge = await bridgeFor(directory, bus);
    const browser = await openBrowser(bridge.port);
    await browser.waitStatus("live");

    browser.publish("req-2", { kind: "ping" });

    const failed = await browser.nextFrame("publish_failed");
    expect(failed.request_id).toBe("req-2");
    expect(failed.indeterminate).toBe(true);
    expect(failed.detail.length).toBeGreaterThan(0);
  });

  it("reports a determinate publish failure when the bus is unreachable", async () => {
    const directory = await tempDir();
    const bus = await makeBus(directory);
    const bridge = await bridgeFor(directory, bus);
    const browser = await openBrowser(bridge.port);
    await browser.waitStatus("live");

    await bus.stop();
    browser.publish("req-3", { kind: "ping" });

    const failed = await browser.nextFrame("publish_failed", 8_000);
    expect(failed.request_id).toBe("req-3");
    expect(failed.indeterminate).toBe(false);
    expect(failed.detail).toContain("cannot connect to the bus");
  });
});

describe("bridge config", () => {
  it("applies the documented defaults", () => {
    const config = parseBridgeConfig({ XDG_RUNTIME_DIR: "/run/user/42" });
    expect(config.socketPath).toBe("/run/user/42/mokmok/bus.sock");
    expect(config.topic).toBe("global");
    expect(config.host).toBe("127.0.0.1");
    expect(config.port).toBe(8787);
    expect(config.subscriberFile).toBe(
      resolve(".mokmok-web", "subscriber-id"),
    );
    expect(config.staticDir).toBe(resolve("packages/console/dist"));

    const withoutRuntimeDir = parseBridgeConfig({});
    expect(withoutRuntimeDir.socketPath).toBe("/tmp/mokmok/bus.sock");
  });

  it("reads overrides and rejects invalid values", () => {
    const topic = `agent:${randomUUID()}`;
    const config = parseBridgeConfig({
      MOKMOK_WEB_SOCKET: "/tmp/custom-bus.sock",
      MOKMOK_WEB_TOPIC: topic,
      MOKMOK_WEB_DATA_DIR: "/data/mokmok",
      MOKMOK_WEB_HOST: "0.0.0.0",
      MOKMOK_WEB_PORT: "9000",
      MOKMOK_WEB_STATIC_DIR: "/srv/console",
    });
    expect(config.socketPath).toBe("/tmp/custom-bus.sock");
    expect(config.topic).toBe(topic);
    expect(config.host).toBe("0.0.0.0");
    expect(config.port).toBe(9000);
    expect(config.subscriberFile).toBe(
      resolve("/data/mokmok", "subscriber-id"),
    );
    expect(config.staticDir).toBe(resolve("/srv/console"));

    expect(() => parseBridgeConfig({ MOKMOK_WEB_PORT: "nope" })).toThrow();
    expect(() => parseBridgeConfig({ MOKMOK_WEB_PORT: "70000" })).toThrow();
    expect(() => parseBridgeConfig({ MOKMOK_WEB_TOPIC: "bogus" })).toThrow();
    expect(() =>
      parseBridgeConfig({ MOKMOK_WEB_TOPIC: "agent:not-a-uuid" }),
    ).toThrow();
  });
});
