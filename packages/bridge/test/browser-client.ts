import { WebSocket, type RawData } from "ws";

import {
  parseBridgeServerFrame,
  type BridgeClientFrame,
  type BridgeServerFrame,
  type ConnectionPhase,
  type Cursor,
} from "@mokmok/web-protocol";

const rawDataToText = (data: RawData): string => {
  if (Array.isArray(data)) return Buffer.concat(data).toString("utf8");
  if (data instanceof ArrayBuffer) return Buffer.from(data).toString("utf8");

  return data.toString("utf8");
};

const sleep = (millis: number) =>
  new Promise<void>((resolve) => setTimeout(resolve, millis));

export interface BrowserCloseInfo {
  readonly code: number;
  readonly reason: string;
}

export type BatchFrame = Extract<BridgeServerFrame, { op: "batch" }>;
export type PublishFrame = Extract<BridgeClientFrame, { op: "publish" }>;

export const batchThrough = (batch: BatchFrame): Cursor => {
  const last = batch.events[batch.events.length - 1];
  if (last !== undefined) return { epoch: batch.window.epoch, seq: last.seq + 1 };
  if (batch.lapsed !== null) {
    return { epoch: batch.window.epoch, seq: batch.lapsed.from };
  }

  return { epoch: batch.window.epoch, seq: batch.window.next };
};

/** A browser-protocol client that records every server frame it receives. */
export class BrowserClient {
  readonly frames: BridgeServerFrame[] = [];

  private readonly socket: WebSocket;
  private readonly consumed = new Set<number>();
  private state: "pending" | "open" | "closed" = "pending";
  private closedInfo: BrowserCloseInfo | null = null;
  private lastError: Error | null = null;

  constructor(port: number) {
    this.socket = new WebSocket(`ws://127.0.0.1:${port}/api/stream`);
    this.socket.on("open", () => {
      if (this.state === "pending") this.state = "open";
    });
    this.socket.on("message", (data: RawData) => {
      const frame = parseBridgeServerFrame(rawDataToText(data));
      if (frame !== null) this.frames.push(frame);
    });
    this.socket.on("close", (code, reason) => {
      this.state = "closed";
      this.closedInfo = { code, reason: reason.toString("utf8") };
    });
    this.socket.on("error", (error: Error) => {
      this.lastError =
        error instanceof Error ? error : new Error(String(error));
    });
  }

  static get closeCodeBusy(): number {
    return 4009;
  }

  /** Opens a browser client, retrying while a previous client's cleanup finishes. */
  static async open(
    port: number,
    options: { readonly retries?: number } = {},
  ): Promise<BrowserClient> {
    const retries = options.retries ?? 60;
    let last: BrowserClient | null = null;

    for (let attempt = 0; attempt < retries; attempt += 1) {
      const client = new BrowserClient(port);
      const deadline = Date.now() + 2_000;
      while (client.state === "pending" && Date.now() < deadline) {
        await sleep(5);
      }

      if (client.state === "open") return client;

      last = client;
      if (client.closedInfo?.code === BrowserClient.closeCodeBusy) {
        await sleep(25);
        continue;
      }

      throw client.lastError ?? new Error("the browser websocket did not open");
    }

    throw (
      last?.lastError ??
      new Error("the browser websocket was always rejected as busy")
    );
  }

  get isOpen(): boolean {
    return this.socket.readyState === WebSocket.OPEN;
  }

  async nextFrame<Op extends BridgeServerFrame["op"]>(
    op: Op,
    timeoutMs = 5_000,
  ): Promise<Extract<BridgeServerFrame, { op: Op }>> {
    const deadline = Date.now() + timeoutMs;

    while (true) {
      for (let index = 0; index < this.frames.length; index += 1) {
        const frame = this.frames[index];
        if (frame === undefined || this.consumed.has(index)) continue;
        if ((frame.op as string) === (op as string)) {
          this.consumed.add(index);
          return frame as Extract<BridgeServerFrame, { op: Op }>;
        }
      }

      if (Date.now() >= deadline) {
        throw new Error(
          `timed out waiting for a "${op}" frame; the browser received ${JSON.stringify(this.frames)}`,
        );
      }
      await sleep(5);
    }
  }

  async waitStatus(
    connection: ConnectionPhase,
    timeoutMs = 5_000,
  ): Promise<Extract<BridgeServerFrame, { op: "status" }>> {
    const deadline = Date.now() + timeoutMs;

    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        throw new Error(`timed out waiting for status "${connection}"`);
      }

      const status = await this.nextFrame("status", remaining);
      if (status.connection === connection) return status;
    }
  }

  async waitClosed(timeoutMs = 5_000): Promise<BrowserCloseInfo> {
    const deadline = Date.now() + timeoutMs;

    while (this.closedInfo === null) {
      if (Date.now() >= deadline) {
        throw new Error("timed out waiting for the browser socket to close");
      }
      await sleep(5);
    }

    return this.closedInfo;
  }

  send(frame: BridgeClientFrame): void {
    if (this.socket.readyState !== WebSocket.OPEN) {
      throw new Error("the browser client is not open");
    }
    this.socket.send(JSON.stringify(frame));
  }

  sendRaw(text: string): void {
    if (this.socket.readyState !== WebSocket.OPEN) {
      throw new Error("the browser client is not open");
    }
    this.socket.send(text);
  }

  ack(batch: BatchFrame, through: Cursor = batchThrough(batch)): void {
    this.send({ op: "ack", batch_id: batch.batch_id, through });
  }

  publish(requestId: string, payload: PublishFrame["payload"]): void {
    this.send({ op: "publish", request_id: requestId, payload });
  }

  async close(code = 1000, reason = "test done"): Promise<BrowserCloseInfo> {
    if (this.closedInfo !== null) return this.closedInfo;

    if (this.socket.readyState === WebSocket.OPEN) {
      this.socket.close(code, reason);
    } else {
      this.socket.terminate();
    }

    return await this.waitClosed();
  }
}
