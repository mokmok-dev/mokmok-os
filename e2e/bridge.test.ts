import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterAll, beforeAll, describe, expect, test } from "vitest";
import type { WebSocket } from "ws";

import {
  encodeBridgeClientFrame,
  parseHealthResponse,
  type BatchFrame,
  type BridgeServerFrame,
  type Cursor,
} from "../packages/protocol/src/index.ts";
import { batchThrough } from "../packages/consumer/src/index.ts";
import { startBridge, type BridgeHandle } from "../packages/bridge/src/index.ts";
import {
  DAEMON_AVAILABLE,
  BUS_TOPIC,
  connectBrowser,
  connectBus,
  openBus,
  publishBus,
  queueBrowserFrames,
  startBus,
  stopBus,
  type BusHarness,
  type FrameQueue,
} from "./harness.ts";

const isBatch = (frame: BridgeServerFrame): frame is BatchFrame => frame.op === "batch";

const waitForClose = (socket: WebSocket): Promise<number> =>
  new Promise((resolve) => {
    socket.once("close", (code: number) => resolve(code));
  });

const waitForStatus = async (frames: FrameQueue, phase: string): Promise<void> => {
  await frames.wait(
    (frame) => frame.op === "status" && frame.connection === phase,
    `status ${phase}`,
  );
};

describe.skipIf(!DAEMON_AVAILABLE)("the bridge against the real bus", () => {
  let harness: BusHarness;
  let scratch: string;
  let staticDir: string;
  let subscriberFile: string;
  let bridge: BridgeHandle | undefined;
  let browser: WebSocket | undefined;
  let frames: FrameQueue | undefined;
  let subscriberId = "";

  beforeAll(async () => {
    harness = await startBus();
    scratch = mkdtempSync(join(tmpdir(), "mokmok-web-bridge-e2e-"));
    staticDir = join(scratch, "static");
    mkdirSync(staticDir, { recursive: true });
    writeFileSync(
      join(staticDir, "index.html"),
      "<!doctype html><html><body><div id=\"marker\">mokmok console</div></body></html>",
    );
    writeFileSync(join(staticDir, "probe.txt"), "static text");
    subscriberFile = join(scratch, "data", "subscriber-id");
  });

  afterAll(async () => {
    browser?.terminate();
    await bridge?.close();
    await stopBus(harness);
    rmSync(scratch, { recursive: true, force: true });
  });

  test("a browser drives the subscription, publishes, and acknowledges a batch", async () => {
    bridge = await startBridge({
      socketPath: harness.socket,
      topic: BUS_TOPIC,
      subscriberFile,
      port: 0,
      staticDir,
    });
    browser = await connectBrowser(bridge.port);
    frames = queueBrowserFrames(browser);

    const hello = await frames.wait((frame) => frame.op === "hello", "hello");
    if (hello.op !== "hello") {
      throw new Error("expected a hello frame");
    }
    expect(hello.topic).toBe(BUS_TOPIC);
    subscriberId = hello.subscriber;
    expect(subscriberId).toMatch(/^[0-9a-f-]{36}$/);
    await waitForStatus(frames, "live");

    const health = await fetch(`http://127.0.0.1:${bridge.port}/api/health`);
    expect(health.status).toBe(200);
    const healthFrame = parseHealthResponse(await health.text());
    expect(healthFrame?.status).toBe("live");
    expect(healthFrame?.subscriber).toBe(subscriberId);
    expect(healthFrame?.topic).toBe(BUS_TOPIC);

    const root = await fetch(`http://127.0.0.1:${bridge.port}/`);
    expect(await root.text()).toContain("mokmok console");
    const probe = await fetch(`http://127.0.0.1:${bridge.port}/probe.txt`);
    expect(await probe.text()).toBe("static text");
    const fallback = await fetch(`http://127.0.0.1:${bridge.port}/events/deep-link`);
    expect(await fallback.text()).toContain("mokmok console");

    const second = await connectBrowser(bridge.port);
    const secondCode = await waitForClose(second);
    expect(secondCode).toBe(4009);

    browser.send(
      encodeBridgeClientFrame({
        op: "publish",
        request_id: "e2e-request-1",
        payload: { turn: 1, source: "web-e2e" },
      }),
    );

    const published = await frames.wait(
      (frame) => frame.op === "published" && frame.request_id === "e2e-request-1",
      "the published receipt",
    );
    if (published.op !== "published") {
      throw new Error("expected a published frame");
    }
    expect(published.seq).toBe(0);

    const batch = await frames.wait(
      (frame) => isBatch(frame) && frame.events.some((event) => event.seq === published.seq),
      "the batch with the published event",
    );
    if (!isBatch(batch)) {
      throw new Error("expected a batch frame");
    }
    const through = batchThrough(batch);
    browser.send(encodeBridgeClientFrame({ op: "ack", batch_id: batch.batch_id, through }));

    const acked = await frames.wait((frame) => frame.op === "acked", "the acked frame");
    if (acked.op !== "acked") {
      throw new Error("expected an acked frame");
    }
    expect(acked.through).toEqual(through);

    frames.stop();
    browser.close();
    await waitForClose(browser);
    await bridge.close();
    bridge = undefined;
  });

  test("a restarted bridge resumes from the acknowledged checkpoint", async () => {
    const publisher = await connectBus(harness.socket);
    try {
      await openBus(publisher, null);
      const receipt = await publishBus(publisher, { turn: 2, source: "web-e2e" });
      expect(receipt.op).toBe("published");
    } finally {
      publisher.terminate();
    }

    bridge = await startBridge({
      socketPath: harness.socket,
      topic: BUS_TOPIC,
      subscriberFile,
      port: 0,
      staticDir,
    });
    expect(bridge.subscriberId).toBe(subscriberId);

    browser = await connectBrowser(bridge.port);
    frames = queueBrowserFrames(browser);
    const hello = await frames.wait((frame) => frame.op === "hello", "hello after restart");
    expect(hello.op === "hello" ? hello.subscriber : null).toBe(subscriberId);
    await waitForStatus(frames, "live");

    const batch = await frames.wait(isBatch, "the resumed batch");
    if (!isBatch(batch)) {
      throw new Error("expected a batch frame");
    }
    expect(batch.events.map((event) => event.payload)).toEqual([
      { turn: 2, source: "web-e2e" },
    ]);

    frames.stop();
    browser.close();
    await waitForClose(browser);
    await bridge.close();
    bridge = undefined;
  });

  test("an unacknowledged batch is replayed to the next bridge", async () => {
    bridge = await startBridge({
      socketPath: harness.socket,
      topic: BUS_TOPIC,
      subscriberFile,
      port: 0,
      staticDir,
    });
    browser = await connectBrowser(bridge.port);
    frames = queueBrowserFrames(browser);
    await frames.wait((frame) => frame.op === "hello", "hello on replay");
    await waitForStatus(frames, "live");

    const first = await frames.wait(isBatch, "the replayed batch");
    if (!isBatch(first)) {
      throw new Error("expected a batch frame");
    }
    expect(first.events.map((event) => event.payload)).toEqual([
      { turn: 2, source: "web-e2e" },
    ]);

    frames.stop();
    browser.close();
    await waitForClose(browser);
    await bridge.close();
    bridge = undefined;

    bridge = await startBridge({
      socketPath: harness.socket,
      topic: BUS_TOPIC,
      subscriberFile,
      port: 0,
      staticDir,
    });
    browser = await connectBrowser(bridge.port);
    frames = queueBrowserFrames(browser);
    await frames.wait((frame) => frame.op === "hello", "hello after replay");
    await waitForStatus(frames, "live");

    const replayed = await frames.wait(isBatch, "the batch replayed again");
    if (!isBatch(replayed)) {
      throw new Error("expected a batch frame");
    }
    expect(replayed.events.map((event) => event.payload)).toEqual([
      { turn: 2, source: "web-e2e" },
    ]);

    const through: Cursor = batchThrough(replayed);
    browser.send(
      encodeBridgeClientFrame({ op: "ack", batch_id: replayed.batch_id, through }),
    );
    await frames.wait((frame) => frame.op === "acked", "the ack after replay");
  });
});
