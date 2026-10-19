import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { connect as netConnect } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

import WebSocket, { type RawData } from "ws";

import {
  encodeBusRequest,
  parseBridgeServerFrame,
  parseBusResponse,
  type BridgeServerFrame,
  type BusRequest,
  type BusResponse,
  type Cursor,
  type Event,
} from "../packages/protocol/src/index.ts";

export const PROJECT_ROOT = join(import.meta.dirname, "..");
export const DAEMON = process.env.MOKMOKD ?? join(PROJECT_ROOT, "target", "debug", "mokmokd");
export const DAEMON_AVAILABLE = existsSync(DAEMON);
export const BUS_TOPIC = "global";

export interface BusHarness {
  readonly socket: string;
  readonly scratch: string;
  readonly daemon: ChildProcess;
}

export const sleep = (millis: number): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, millis));

export const waitFor = async (predicate: () => boolean, label: string): Promise<void> => {
  for (let attempt = 0; attempt < 400; attempt += 1) {
    if (predicate()) {
      return;
    }
    await sleep(25);
  }
  throw new Error(`timed out waiting for ${label}`);
};

export const startBus = async (): Promise<BusHarness> => {
  const scratch = mkdtempSync(join(tmpdir(), "mokmok-web-e2e-"));
  const socket = join(scratch, "bus.sock");
  const daemon = spawn(
    DAEMON,
    ["--data-dir", join(scratch, "data"), "serve", "--socket", socket],
    {
      env: { ...process.env, OLLAMA_API_KEY: "unused-by-the-bus" },
      stdio: ["ignore", "ignore", "pipe"],
    },
  );
  let stderr = "";
  daemon.stderr?.on("data", (chunk: Buffer) => {
    stderr += chunk.toString();
  });

  try {
    await waitFor(() => existsSync(socket), `the bus socket (${stderr})`);
  } catch (error) {
    daemon.kill("SIGTERM");
    throw error;
  }

  return { socket, scratch, daemon };
};

export const stopBus = async (harness: BusHarness): Promise<void> => {
  harness.daemon.kill("SIGTERM");
  await new Promise<void>((resolve) => {
    if (harness.daemon.exitCode !== null) {
      resolve();
      return;
    }
    harness.daemon.once("exit", () => resolve());
    setTimeout(resolve, 3000);
  });
  rmSync(harness.scratch, { recursive: true, force: true });
};

export const connectBus = (socket: string): Promise<WebSocket> =>
  new Promise((resolve, reject) => {
    const client = new WebSocket("ws://localhost/ws", {
      createConnection: () => netConnect(socket),
    });
    client.once("open", () => resolve(client));
    client.once("error", reject);
  });

export const nextBusFrame = (client: WebSocket): Promise<BusResponse> =>
  new Promise((resolve, reject) => {
    const onMessage = (data: RawData): void => {
      cleanup();
      const frame = parseBusResponse(data.toString());
      if (frame === null) {
        reject(new Error(`the bus sent a frame the protocol cannot read: ${data.toString()}`));
        return;
      }
      resolve(frame);
    };
    const onError = (error: Error): void => {
      cleanup();
      reject(error);
    };
    const cleanup = (): void => {
      client.off("message", onMessage);
      client.off("error", onError);
    };
    client.on("message", onMessage);
    client.on("error", onError);
  });

export const sendBus = async (client: WebSocket, request: BusRequest): Promise<BusResponse> => {
  const reply = nextBusFrame(client);
  client.send(encodeBusRequest(request));

  return reply;
};

export const openBus = (client: WebSocket, subscriber: string | null): Promise<BusResponse> =>
  sendBus(client, { op: "open", topic: BUS_TOPIC, subscriber });

export const publishBus = async (client: WebSocket, payload: unknown): Promise<BusResponse> =>
  sendBus(client, { op: "publish", payload } as BusRequest);

export const cursorAfter = (
  events: readonly Event[],
  window: { epoch: string; next: number },
): Cursor => {
  const last = events.at(-1);

  return last === undefined
    ? { epoch: window.epoch, seq: window.next }
    : { epoch: window.epoch, seq: last.seq + 1 };
};

export interface FrameQueue {
  readonly wait: (
    predicate: (frame: BridgeServerFrame) => boolean,
    label: string,
    timeoutMs?: number,
  ) => Promise<BridgeServerFrame>;
  readonly received: () => readonly BridgeServerFrame[];
  readonly stop: () => void;
}

/**
 * Frames that no waiter claims stay queued, so a test can wait for a later frame while an earlier
 * one is still buffered.
 */
export const queueBrowserFrames = (socket: WebSocket): FrameQueue => {
  const buffered: BridgeServerFrame[] = [];
  const waiters: Array<{
    readonly predicate: (frame: BridgeServerFrame) => boolean;
    readonly resolve: (frame: BridgeServerFrame) => void;
  }> = [];

  const onMessage = (data: RawData): void => {
    const frame = parseBridgeServerFrame(data.toString());
    if (frame === null) {
      return;
    }

    const index = waiters.findIndex((waiter) => waiter.predicate(frame));
    if (index >= 0) {
      const [waiter] = waiters.splice(index, 1);
      waiter?.resolve(frame);
      return;
    }

    buffered.push(frame);
  };

  socket.on("message", onMessage);

  return {
    wait: (predicate, label, timeoutMs = 8000) => {
      const index = buffered.findIndex(predicate);
      if (index >= 0) {
        const [frame] = buffered.splice(index, 1);
        if (frame !== undefined) {
          return Promise.resolve(frame);
        }
      }

      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          const position = waiters.indexOf(waiter);
          if (position >= 0) {
            waiters.splice(position, 1);
          }
          reject(
            new Error(
              `timed out waiting for ${label}; received ${buffered.map((frame) => frame.op).join(", ")}`,
            ),
          );
        }, timeoutMs);
        const waiter = {
          predicate,
          resolve: (frame: BridgeServerFrame): void => {
            clearTimeout(timer);
            resolve(frame);
          },
        };
        waiters.push(waiter);
      });
    },
    received: () => buffered,
    stop: () => {
      socket.off("message", onMessage);
    },
  };
};

export const connectBrowser = (port: number): Promise<WebSocket> =>
  new Promise((resolve, reject) => {
    const socket = new WebSocket(`ws://127.0.0.1:${port}/api/stream`);
    socket.once("open", () => resolve(socket));
    socket.once("error", reject);
  });
