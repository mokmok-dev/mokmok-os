import { randomUUID } from "node:crypto";
import { unlink } from "node:fs/promises";
import { createServer, type Server } from "node:http";

import { WebSocket, WebSocketServer, type RawData } from "ws";

import {
  decodeBusRequest,
  type AckRequest,
  type BatchResponse,
  type BusRequest,
  type Event,
  type OpenRequest,
  type PublishRequest,
  type PullRequest,
  type Refusal,
  type Window,
} from "@mokmok/web-protocol";

const rawDataToText = (data: RawData): string => {
  if (Array.isArray(data)) return Buffer.concat(data).toString("utf8");
  if (data instanceof ArrayBuffer) return Buffer.from(data).toString("utf8");

  return data.toString("utf8");
};

const instantAt = (millis: number) => ({
  secs_since_epoch: Math.floor(millis / 1_000),
  nanos_since_epoch: (millis % 1_000) * 1_000_000,
});

export interface RecordedBusRequest {
  readonly connection: number;
  readonly request: BusRequest;
}

export interface FakeBusConnection {
  readonly id: number;
  readonly socket: WebSocket;
  subscriber: string | null;
  pendingPull: { readonly max: number } | null;
}

export interface FakeBusOptions {
  readonly topic?: string;
  readonly refuseOpen?: Refusal;
  /** "drop" closes the connection without answering; "silence" never answers. */
  readonly onPublish?: (
    request: PublishRequest,
  ) => "respond" | "drop" | "silence";
}

/**
 * A real WebSocket server on a Unix socket that speaks the observed bus wire
 * protocol, keeps a per-subscriber checkpoint, and long-polls pulls.
 */
export class FakeBus {
  readonly socketPath: string;
  readonly epoch = randomUUID();
  readonly requests: RecordedBusRequest[] = [];
  readonly events: Event[] = [];
  readonly checkpoints = new Map<string, number>();
  connectionCount = 0;

  private readonly server: Server;
  private readonly wss: WebSocketServer;
  private readonly options: FakeBusOptions;
  private readonly connections = new Set<FakeBusConnection>();
  private stopped = false;

  constructor(socketPath: string, options: FakeBusOptions = {}) {
    this.socketPath = socketPath;
    this.options = options;
    this.server = createServer();
    this.wss = new WebSocketServer({ server: this.server });
    this.wss.on("connection", (socket) => this.onConnection(socket));
  }

  get topic(): string {
    return this.options.topic ?? "global";
  }

  get openRequests(): readonly OpenRequest[] {
    return this.requests.flatMap(({ request }) =>
      request.op === "open" ? [request] : [],
    );
  }

  get pullRequests(): readonly PullRequest[] {
    return this.requests.flatMap(({ request }) =>
      request.op === "pull" ? [request] : [],
    );
  }

  get ackRequests(): readonly AckRequest[] {
    return this.requests.flatMap(({ request }) =>
      request.op === "ack" ? [request] : [],
    );
  }

  get publishRequests(): readonly PublishRequest[] {
    return this.requests.flatMap(({ request }) =>
      request.op === "publish" ? [request] : [],
    );
  }

  async start(): Promise<void> {
    await new Promise<void>((resolve, reject) => {
      const onError = (error: Error) => {
        this.server.off("listening", onListening);
        reject(error);
      };
      const onListening = () => {
        this.server.off("error", onError);
        resolve();
      };
      this.server.once("error", onError);
      this.server.once("listening", onListening);
      this.server.listen(this.socketPath);
    });
  }

  async stop(): Promise<void> {
    if (this.stopped) return;
    this.stopped = true;

    for (const connection of this.connections) {
      try {
        connection.socket.terminate();
      } catch {
        // The socket was already gone.
      }
    }
    this.connections.clear();

    await new Promise<void>((resolve) => {
      this.wss.close(() => resolve());
    });
    await new Promise<void>((resolve) => {
      this.server.close(() => resolve());
    });
    await unlink(this.socketPath).catch(() => {});
  }

  emit(payload: Event["payload"], topic: string = this.topic): Event {
    const event: Event = {
      id: randomUUID(),
      topic,
      seq: this.events.length,
      at: instantAt(Date.now()),
      payload,
    };
    this.events.push(event);
    this.flushAll();

    return event;
  }

  private onConnection(socket: WebSocket): void {
    this.connectionCount += 1;
    const connection: FakeBusConnection = {
      id: this.connectionCount,
      socket,
      subscriber: null,
      pendingPull: null,
    };
    this.connections.add(connection);

    socket.on("error", () => {});
    socket.on("close", () => {
      connection.pendingPull = null;
      this.connections.delete(connection);
    });
    socket.on("message", (data: RawData) => {
      this.onMessage(connection, data);
    });
  }

  private onMessage(connection: FakeBusConnection, data: RawData): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(rawDataToText(data));
    } catch {
      connection.socket.close(1002, "invalid JSON");
      return;
    }

    const request = decodeBusRequest(parsed);
    if (request === null) {
      connection.socket.close(1002, "invalid request");
      return;
    }

    this.requests.push({ connection: connection.id, request });

    switch (request.op) {
      case "open":
        this.onOpen(connection, request);
        break;
      case "pull":
        connection.pendingPull = { max: request.max };
        this.flush(connection);
        break;
      case "ack":
        this.onAck(connection, request);
        break;
      case "publish":
        this.onPublish(connection, request);
        break;
    }
  }

  private onOpen(connection: FakeBusConnection, request: OpenRequest): void {
    if (this.options.refuseOpen !== undefined) {
      this.send(connection, { op: "refused", reason: this.options.refuseOpen });
      return;
    }

    // A null subscriber names a publisher connection, which owns no checkpoint.
    if (request.subscriber !== null) {
      connection.subscriber = request.subscriber;
      if (!this.checkpoints.has(request.subscriber)) {
        this.checkpoints.set(request.subscriber, 0);
      }
    }

    this.send(connection, { op: "opened", window: this.window() });
    this.flush(connection);
  }

  private onAck(connection: FakeBusConnection, request: AckRequest): void {
    if (connection.subscriber !== null) {
      this.checkpoints.set(connection.subscriber, request.through.seq);
    }

    this.send(connection, { op: "acked", through: request.through });
    this.flushAll();
  }

  private onPublish(
    connection: FakeBusConnection,
    request: PublishRequest,
  ): void {
    const mode = this.options.onPublish?.(request) ?? "respond";
    if (mode === "drop") {
      connection.socket.close(1011, "publish dropped");
      return;
    }
    if (mode === "silence") return;

    const event = this.emit(request.payload);
    this.send(connection, {
      op: "published",
      id: event.id,
      seq: event.seq,
      at: event.at,
    });
  }

  private flushAll(): void {
    for (const connection of this.connections) this.flush(connection);
  }

  private flush(connection: FakeBusConnection): void {
    if (connection.pendingPull === null || connection.subscriber === null) {
      return;
    }

    const checkpoint = this.checkpoints.get(connection.subscriber) ?? 0;
    if (checkpoint >= this.events.length) return;

    const events = this.events.slice(
      checkpoint,
      checkpoint + connection.pendingPull.max,
    );
    connection.pendingPull = null;

    const batch: BatchResponse = {
      op: "batch",
      window: this.window(),
      lapsed: null,
      events,
    };
    this.send(connection, batch);
  }

  private window(): Window {
    return { epoch: this.epoch, first: 0, next: this.events.length };
  }

  private send(connection: FakeBusConnection, value: unknown): void {
    connection.socket.send(JSON.stringify(value));
  }
}
