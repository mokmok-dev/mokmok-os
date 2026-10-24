import { spawn, type ChildProcess } from "node:child_process";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, it } from "vitest";
import { connect, type TestClient, takeOp } from "./support/client.ts";
import { tempDir, tempSocket } from "./support/helpers.ts";

const fixture = fileURLToPath(new URL("./fixtures/server.ts", import.meta.url));

const delay = (millis: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(resolve, millis);
  });

const startServer = async (socketPath: string, logPath: string): Promise<ChildProcess> => {
  const child = spawn(process.execPath, [fixture, socketPath, logPath], {
    stdio: ["ignore", "inherit", "inherit"],
  });
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    try {
      const client = await connect(socketPath, 500);
      client.stop();
      return child;
    } catch {
      await delay(50);
    }
  }
  child.kill("SIGKILL");
  throw new Error("fixture server never accepted a connection");
};

const stop = async (child: ChildProcess, signal: NodeJS.Signals): Promise<void> => {
  const exited = new Promise<void>((resolve) => {
    child.once("exit", () => resolve());
  });
  child.kill(signal);
  await exited;
};

const publishAndWait = async (client: TestClient, payload: unknown): Promise<void> => {
  client.send({ op: "publish", req: 1, topic: "durable", payload });
  await client.take(takeOp("published"));
};

it("crash replays an acknowledged event from the log after SIGKILL", async () => {
  const dir = await tempDir();
  const socketPath = tempSocket();
  const logPath = join(dir, "bus.jsonl");
  const running: Array<ChildProcess> = [];

  const start = async (): Promise<ChildProcess> => {
    const child = await startServer(socketPath, logPath);
    running.push(child);
    return child;
  };

  try {
    const first = await start();
    const publisher = await connect(socketPath);
    await publisher.take(takeOp("hello"));
    await publishAndWait(publisher, { text: "survives" });
    publisher.stop();
    await stop(first, "SIGKILL");

    const second = await start();
    const subscriber = await connect(socketPath);
    expect(await subscriber.take(takeOp("hello"))).toEqual({ op: "hello", head: 1 });
    subscriber.send({ op: "subscribe", req: 2, topic: "durable", after: 0 });
    expect(await subscriber.take(takeOp("event"))).toEqual({
      op: "event",
      seq: 1,
      topic: "durable",
      payload: { text: "survives" },
    });
    subscriber.stop();
    await stop(second, "SIGKILL");
  } finally {
    for (const child of running) {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill("SIGKILL");
      }
    }
  }
});
