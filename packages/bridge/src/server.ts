import { randomUUID } from "node:crypto";
import {
  createServer,
  type IncomingMessage,
  type Server,
  type ServerResponse,
} from "node:http";

import {
  Cause,
  Deferred,
  Duration,
  Effect,
  Exit,
  Fiber,
  Option,
  Queue,
  Ref,
  Schedule,
  Scope,
} from "effect";
import { WebSocket, WebSocketServer } from "ws";

import {
  describeRefusal,
  parseBridgeClientFrame,
  type BridgeClientFrame,
  type BridgeServerFrame,
  type ConnectionPhase,
  type Cursor,
  type Event,
  type HealthResponse,
  type Lapsed,
  type PublishFrame,
  type Window,
} from "@mokmok/web-protocol";

import { connectBus, type BusConnection } from "./bus.ts";
import { BusTransportError, BridgeStartError, BrowserSendError } from "./errors.ts";
import { makePublisher, type Publisher } from "./publisher.ts";
import { rawDataToText } from "./raw.ts";
import { respondText, serveStatic } from "./static.ts";
import { loadSubscriberId } from "./subscriber.ts";
import type {
  BridgeHandle,
  BridgeOptions,
  BridgeState,
} from "./types.ts";

const BATCH_MAX = 128;
const PULL_BACKOFF_BASE = "100 millis";
const PULL_BACKOFF_CAP = Duration.seconds(5);
const MAX_PAYLOAD_BYTES = 1024 * 1024;
const SEND_QUEUE_CAPACITY = 64;

const SECOND_CLIENT_CLOSE_CODE = 4009;
const INVALID_FRAME_CLOSE_CODE = 4002;
const ACK_MISMATCH_CLOSE_CODE = 4003;
const UNKNOWN_WEBSOCKET_PATH_CLOSE_CODE = 4004;

const INITIAL_STATE: BridgeState = {
  status: "offline",
  eventsDelivered: 0,
  lastSeq: null,
  pendingBatch: null,
  connectedAtMs: null,
};

type ClientEvent =
  | { readonly _tag: "frame"; readonly frame: BridgeClientFrame }
  | { readonly _tag: "invalid" }
  | { readonly _tag: "closed" };

interface IncomingClient {
  readonly socket: WebSocket;
  readonly inbox: ClientInbox;
}

interface PendingAck {
  readonly batchId: string;
  readonly deferred: Deferred.Deferred<Cursor>;
}

interface ClientContext {
  readonly options: BridgeOptions;
  readonly subscriberId: string;
  readonly state: Ref.Ref<BridgeState>;
  readonly publisher: Publisher;
  readonly socket: WebSocket;
  readonly sendQueue: Queue.Queue<BridgeServerFrame>;
  readonly pendingAck: Ref.Ref<Option.Option<PendingAck>>;
}

type SubscriptionOutcome =
  | { readonly _tag: "refused"; readonly detail: string }
  | { readonly _tag: "offline"; readonly detail: string };

interface HttpServerResources {
  readonly server: Server;
  readonly wss: WebSocketServer;
}

interface BridgeReady {
  readonly port: number;
}

class ClientInbox {
  private sink: ((event: ClientEvent) => void) | null = null;
  private readonly buffered: ClientEvent[] = [];

  push(event: ClientEvent): void {
    const sink = this.sink;
    if (sink !== null) {
      sink(event);
      return;
    }

    this.buffered.push(event);
  }

  attach(sink: (event: ClientEvent) => void): void {
    this.sink = sink;
    while (this.buffered.length > 0) {
      const event = this.buffered.shift();
      if (event !== undefined) sink(event);
    }
  }
}

const patchState = (
  state: Ref.Ref<BridgeState>,
  patch: Partial<BridgeState>,
): Effect.Effect<void> =>
  Ref.update(state, (current) => ({ ...current, ...patch }));

const batchThrough = (
  window: Window,
  lapsed: Lapsed | null,
  events: readonly Event[],
): Cursor => {
  const last = events[events.length - 1];
  if (last !== undefined) return { epoch: window.epoch, seq: last.seq + 1 };
  if (lapsed !== null) return { epoch: window.epoch, seq: lapsed.from };

  return { epoch: window.epoch, seq: window.next };
};

const cursorEquals = (left: Cursor, right: Cursor): boolean =>
  left.epoch === right.epoch && left.seq === right.seq;

const retrySchedule = Schedule.exponential(PULL_BACKOFF_BASE, 2).pipe(
  Schedule.jittered,
  Schedule.modifyDelay(({ duration }) =>
    Effect.succeed(
      Duration.min(Duration.fromInputUnsafe(duration), PULL_BACKOFF_CAP),
    ),
  ),
);

const closeSocketSync = (
  socket: WebSocket,
  code: number,
  reason: string,
): void => {
  try {
    if (socket.readyState === WebSocket.OPEN) {
      socket.close(code, reason);
    } else if (socket.readyState !== WebSocket.CLOSED) {
      socket.terminate();
    }
  } catch {
    try {
      socket.terminate();
    } catch {
      // The socket was already gone.
    }
  }
};

const closeBrowserSocket = (
  socket: WebSocket,
  code: number,
  reason: string,
): Effect.Effect<void> => Effect.sync(() => closeSocketSync(socket, code, reason));

const sendBrowserText = (
  socket: WebSocket,
  text: string,
): Effect.Effect<void, BrowserSendError> =>
  Effect.callback<void, BrowserSendError>((resume) => {
    if (socket.readyState !== WebSocket.OPEN) {
      resume(
        Effect.fail(
          new BrowserSendError({ detail: "the browser connection is not open" }),
        ),
      );
      return;
    }

    try {
      socket.send(text, (error?: Error) => {
        if (error == null) {
          resume(Effect.void);
        } else {
          resume(
            Effect.fail(
              new BrowserSendError({
                detail: `could not write to the browser: ${error.message}`,
              }),
            ),
          );
        }
      });
    } catch (error) {
      resume(
        Effect.fail(
          new BrowserSendError({
            detail: `could not write to the browser: ${String(error)}`,
          }),
        ),
      );
    }
  });

const writeLoop = (
  socket: WebSocket,
  queue: Queue.Queue<BridgeServerFrame>,
): Effect.Effect<void> =>
  Effect.gen(function* () {
    while (true) {
      const frame = yield* Queue.take(queue);
      const result = yield* sendBrowserText(socket, JSON.stringify(frame)).pipe(
        Effect.exit,
      );

      if (Exit.isFailure(result)) {
        closeSocketSync(socket, 1011, "browser write failed");
        return;
      }
    }
  });

const sendStatus = (
  context: ClientContext,
  connection: ConnectionPhase,
  detail: string | null,
): Effect.Effect<void> =>
  Queue.offer(context.sendQueue, { op: "status", connection, detail });

const handlePublish = (
  context: ClientContext,
  frame: PublishFrame,
): Effect.Effect<void> =>
  Effect.gen(function* () {
    const outcome = yield* context.publisher.publish(frame.payload).pipe(
      Effect.map((receipt) => ({ _tag: "published" as const, receipt })),
      Effect.catch((error) =>
        Effect.succeed({ _tag: "failed" as const, error }),
      ),
    );

    if (outcome._tag === "published") {
      yield* Queue.offer(context.sendQueue, {
        op: "published",
        request_id: frame.request_id,
        id: outcome.receipt.id,
        seq: outcome.receipt.seq,
        at: outcome.receipt.at,
      });
      return;
    }

    yield* Queue.offer(context.sendQueue, {
      op: "publish_failed",
      request_id: frame.request_id,
      detail: outcome.error.detail,
      indeterminate: outcome.error.indeterminate,
    });
  });

const frameLoop = (
  context: ClientContext,
  events: Queue.Queue<ClientEvent>,
): Effect.Effect<void> =>
  Effect.gen(function* () {
    while (true) {
      const event = yield* Queue.take(events);

      if (event._tag === "closed") return;

      if (event._tag === "invalid") {
        yield* closeBrowserSocket(
          context.socket,
          INVALID_FRAME_CLOSE_CODE,
          "invalid bridge frame",
        );
        return;
      }

      if (event.frame.op === "ack") {
        const pending = yield* Ref.get(context.pendingAck);
        if (
          Option.isSome(pending) &&
          pending.value.batchId === event.frame.batch_id
        ) {
          yield* Deferred.succeed(
            pending.value.deferred,
            event.frame.through,
          );
        }
        continue;
      }

      // A publish round-trip must never stall the batch/ack conversation.
      yield* Effect.forkChild(handlePublish(context, event.frame));
    }
  });

const pullLoop = (
  context: ClientContext,
  connection: BusConnection,
): Effect.Effect<never, BusTransportError> =>
  Effect.gen(function* () {
    while (true) {
      yield* connection.send({ op: "pull", max: BATCH_MAX });

      const response = yield* Queue.take(connection.responses);
      if (response.op === "refused") {
        return yield* Effect.fail(
          new BusTransportError({
            detail: `the bus refused a pull: ${describeRefusal(response.reason)}`,
          }),
        );
      }
      if (response.op !== "batch") {
        return yield* Effect.fail(
          new BusTransportError({
            detail: `the bus answered a pull with "${response.op}"`,
          }),
        );
      }

      const events = response.events;
      yield* Ref.update(context.state, (current) => {
        const last = events[events.length - 1];
        return {
          ...current,
          eventsDelivered: current.eventsDelivered + events.length,
          lastSeq: last === undefined ? current.lastSeq : last.seq,
        };
      });

      const batchId = randomUUID();
      const through = batchThrough(response.window, response.lapsed, events);
      const acknowledged = yield* Deferred.make<Cursor>();
      yield* Ref.set(
        context.pendingAck,
        Option.some({ batchId, deferred: acknowledged }),
      );

      yield* Queue.offer(context.sendQueue, {
        op: "batch",
        batch_id: batchId,
        window: response.window,
        lapsed: response.lapsed,
        events,
      });
      yield* patchState(context.state, { pendingBatch: batchId });

      const cursor = yield* Deferred.await(acknowledged);
      yield* Ref.set(context.pendingAck, Option.none());

      if (!cursorEquals(cursor, through)) {
        yield* closeBrowserSocket(
          context.socket,
          ACK_MISMATCH_CLOSE_CODE,
          "batch ack did not match the delivered position",
        );
        return yield* Effect.fail(
          new BusTransportError({
            detail: "the browser acknowledged a cursor the bridge did not deliver",
          }),
        );
      }

      // The bus checkpoint advances only after the browser persisted and acked
      // the batch, so a crash here replays rather than loses the batch.
      yield* connection.send({ op: "ack", through });

      const acked = yield* Queue.take(connection.responses);
      if (acked.op !== "acked" || !cursorEquals(acked.through, through)) {
        return yield* Effect.fail(
          new BusTransportError({
            detail: `the bus answered an ack with "${acked.op}" at the wrong position`,
          }),
        );
      }

      yield* Queue.offer(context.sendQueue, {
        op: "acked",
        through: acked.through,
      });
      yield* patchState(context.state, { pendingBatch: null });
    }
  });

const attemptSubscription = (
  context: ClientContext,
): Effect.Effect<SubscriptionOutcome> =>
  Effect.scoped(
    Effect.gen(function* () {
      const connection = yield* Effect.acquireRelease(
        connectBus(context.options.socketPath),
        (bus) => bus.close(),
      );

      yield* connection.send({
        op: "open",
        topic: context.options.topic,
        subscriber: context.subscriberId,
      });

      const opening = yield* Queue.take(connection.responses);
      if (opening.op === "refused") {
        return {
          _tag: "refused",
          detail: describeRefusal(opening.reason),
        } satisfies SubscriptionOutcome;
      }
      if (opening.op !== "opened") {
        return {
          _tag: "offline",
          detail: `the bus answered open with "${opening.op}"`,
        } satisfies SubscriptionOutcome;
      }

      yield* patchState(context.state, {
        status: "live",
        connectedAtMs: Date.now(),
        pendingBatch: null,
      });
      yield* sendStatus(context, "live", null);
      yield* pullLoop(context, connection);

      return {
        _tag: "offline",
        detail: "the subscription ended",
      } satisfies SubscriptionOutcome;
    }),
  ).pipe(
    Effect.catch((error: BusTransportError) =>
      Effect.succeed<SubscriptionOutcome>({
        _tag: "offline",
        detail: error.detail,
      }),
    ),
  );

const subscriptionLoop = (context: ClientContext): Effect.Effect<never> =>
  Effect.gen(function* () {
    const backoff = yield* Schedule.toStepWithSleep(retrySchedule);
    let first = true;

    while (true) {
      if (!first) {
        yield* patchState(context.state, {
          status: "connecting",
          pendingBatch: null,
          connectedAtMs: null,
        });
        yield* sendStatus(context, "connecting", null);
      }
      first = false;

      const outcome = yield* attemptSubscription(context);
      if (outcome._tag === "refused") {
        yield* patchState(context.state, {
          status: "refused",
          pendingBatch: null,
          connectedAtMs: null,
        });
        yield* sendStatus(context, "refused", outcome.detail);
      } else {
        yield* patchState(context.state, {
          status: "offline",
          pendingBatch: null,
          connectedAtMs: null,
        });
        yield* sendStatus(context, "offline", outcome.detail);
      }

      yield* backoff(undefined).pipe(Effect.ignore);
    }
  });

const cleanupSession = (
  state: Ref.Ref<BridgeState>,
  socket: WebSocket,
): Effect.Effect<void> =>
  Effect.gen(function* () {
    yield* closeBrowserSocket(socket, 1000, "session ended");
    yield* patchState(state, {
      status: "offline",
      pendingBatch: null,
      connectedAtMs: null,
    });
  });

const runClientSession = (
  options: BridgeOptions,
  subscriberId: string,
  state: Ref.Ref<BridgeState>,
  publisher: Publisher,
  client: IncomingClient,
): Effect.Effect<void> =>
  Effect.gen(function* () {
    const events = yield* Queue.unbounded<ClientEvent>();
    client.inbox.attach((event) => {
      Queue.offerUnsafe(events, event);
    });

    const sendQueue = yield* Queue.bounded<BridgeServerFrame>(
      SEND_QUEUE_CAPACITY,
    );
    const pendingAck = yield* Ref.make<Option.Option<PendingAck>>(Option.none());

    yield* Effect.forkChild(writeLoop(client.socket, sendQueue));

    const context: ClientContext = {
      options,
      subscriberId,
      state,
      publisher,
      socket: client.socket,
      sendQueue,
      pendingAck,
    };

    yield* patchState(state, {
      status: "connecting",
      pendingBatch: null,
      connectedAtMs: null,
    });
    yield* Queue.offer(sendQueue, {
      op: "hello",
      topic: options.topic,
      subscriber: subscriberId,
    });
    yield* sendStatus(context, "connecting", null);

    yield* Effect.race(
      subscriptionLoop(context),
      frameLoop(context, events),
    );
  }).pipe(Effect.ensuring(cleanupSession(state, client.socket)));

const acceptLoop = (
  options: BridgeOptions,
  subscriberId: string,
  state: Ref.Ref<BridgeState>,
  publisher: Publisher,
  connections: Queue.Queue<IncomingClient>,
): Effect.Effect<never> =>
  Effect.gen(function* () {
    const active = yield* Ref.make(false);

    while (true) {
      const client = yield* Queue.take(connections);

      if (yield* Ref.get(active)) {
        yield* closeBrowserSocket(
          client.socket,
          SECOND_CLIENT_CLOSE_CODE,
          "another browser is already connected",
        );
        continue;
      }

      yield* Ref.set(active, true);

      const session = runClientSession(
        options,
        subscriberId,
        state,
        publisher,
        client,
      ).pipe(
        Effect.ensuring(Ref.set(active, false)),
        Effect.catchCause(() => Effect.void),
      );
      yield* Effect.forkChild(session);
    }
  });

const respondJson = (
  response: ServerResponse,
  method: string,
  document: unknown,
): void => {
  const body = Buffer.from(JSON.stringify(document));
  response.writeHead(200, {
    "content-type": "application/json; charset=utf-8",
    "content-length": body.byteLength,
    "cache-control": "no-store",
  });
  response.end(method === "HEAD" ? undefined : body);
};

const handleHttpRequest = async (
  request: IncomingMessage,
  response: ServerResponse,
  options: BridgeOptions,
  state: Ref.Ref<BridgeState>,
  subscriberId: string,
): Promise<void> => {
  const method = request.method ?? "GET";
  const url = new URL(request.url ?? "/", "http://localhost");

  if (url.pathname === "/api/health") {
    if (method !== "GET" && method !== "HEAD") {
      respondText(response, method, 405, "method not allowed");
      return;
    }

    const snapshot = Effect.runSync(Ref.get(state));
    const health: HealthResponse = {
      status: snapshot.status,
      topic: options.topic,
      subscriber: subscriberId,
      socket: options.socketPath,
      events_delivered: snapshot.eventsDelivered,
      last_seq: snapshot.lastSeq,
      pending_batch: snapshot.pendingBatch,
      connected_at_ms: snapshot.connectedAtMs,
    };
    respondJson(response, method, health);
    return;
  }

  if (url.pathname.startsWith("/api/")) {
    respondText(response, method, 404, "not found");
    return;
  }

  if (method !== "GET" && method !== "HEAD") {
    respondText(response, method, 405, "method not allowed");
    return;
  }

  await serveStatic(
    method,
    response,
    request.url ?? "/",
    options.staticDir,
  );
};

const acquireHttpServer = (
  options: BridgeOptions,
  state: Ref.Ref<BridgeState>,
  connections: Queue.Queue<IncomingClient>,
  subscriberId: string,
): Effect.Effect<HttpServerResources, BridgeStartError> =>
  Effect.gen(function* () {
    const server = createServer((request, response) => {
      void handleHttpRequest(request, response, options, state, subscriberId);
    });
    const wss = new WebSocketServer({
      server,
      maxPayload: MAX_PAYLOAD_BYTES,
      perMessageDeflate: false,
    });

    wss.on("connection", (socket, request) => {
      const pathname = new URL(request.url ?? "/", "http://localhost").pathname;
      if (pathname !== "/api/stream") {
        closeSocketSync(
          socket,
          UNKNOWN_WEBSOCKET_PATH_CLOSE_CODE,
          "unknown websocket endpoint",
        );
        return;
      }

      const inbox = new ClientInbox();
      socket.on("message", (data) => {
        const frame = parseBridgeClientFrame(rawDataToText(data));
        inbox.push(
          frame === null ? { _tag: "invalid" } : { _tag: "frame", frame },
        );
      });
      socket.on("close", () => {
        inbox.push({ _tag: "closed" });
      });
      socket.on("error", () => {
        inbox.push({ _tag: "closed" });
      });

      Queue.offerUnsafe(connections, { socket, inbox });
    });

    const host = options.host ?? "127.0.0.1";
    yield* Effect.callback<void, BridgeStartError>((resume) => {
      let settled = false;
      const cleanup = () => {
        server.off("error", onError);
        server.off("listening", onListening);
      };
      const onError = (error: Error) => {
        if (settled) return;
        settled = true;
        cleanup();
        try {
          wss.close();
        } catch {
          // The server never owned a listening socket.
        }
        try {
          server.close();
        } catch {
          // The server never owned a listening socket.
        }
        resume(
          Effect.fail(
            new BridgeStartError({
              detail: `cannot listen on ${host}:${options.port}: ${error.message}`,
            }),
          ),
        );
      };
      const onListening = () => {
        if (settled) return;
        settled = true;
        cleanup();
        resume(Effect.void);
      };

      server.once("error", onError);
      server.once("listening", onListening);
      try {
        server.listen(options.port, host);
      } catch (error) {
        onError(error instanceof Error ? error : new Error(String(error)));
      }

      return Effect.sync(() => {
        cleanup();
        try {
          server.close();
        } catch {
          // The server never owned a listening socket.
        }
      });
    });

    return { server, wss };
  });

const releaseHttpServer = ({
  server,
  wss,
}: HttpServerResources): Effect.Effect<void> =>
  Effect.callback<void>((resume) => {
    // A browser that ignores the closing handshake must not hold up shutdown.
    const force = setTimeout(() => {
      for (const client of wss.clients) {
        try {
          client.terminate();
        } catch {
          // The socket was already gone.
        }
      }
    }, 1000);
    force.unref?.();

    for (const client of wss.clients) {
      closeSocketSync(client, 1001, "bridge shutting down");
    }

    try {
      wss.close();
    } catch {
      // The websocket server was already closed.
    }

    server.close(() => {
      clearTimeout(force);
      resume(Effect.void);
    });
  });

const runBridge = (
  options: BridgeOptions,
  subscriberId: string,
  ready: Deferred.Deferred<BridgeReady, Error>,
): Effect.Effect<void, BridgeStartError> =>
  Effect.scoped(
    Effect.gen(function* () {
      const state = yield* Ref.make<BridgeState>(INITIAL_STATE);
      const connections = yield* Queue.unbounded<IncomingClient>();

      const publisherScope = yield* Scope.make();
      yield* Effect.addFinalizer(() =>
        Scope.close(publisherScope, Exit.succeed(undefined)),
      );
      const publisher = yield* makePublisher(
        options.socketPath,
        options.topic,
        publisherScope,
      );

      const { server } = yield* Effect.acquireRelease(
        acquireHttpServer(options, state, connections, subscriberId),
        releaseHttpServer,
      );

      const address = server.address();
      const port =
        typeof address === "object" && address !== null
          ? address.port
          : options.port;
      yield* Deferred.succeed(ready, { port });

      yield* acceptLoop(
        options,
        subscriberId,
        state,
        publisher,
        connections,
      );
    }),
  );

export const startBridge = async (
  options: BridgeOptions,
): Promise<BridgeHandle> => {
  const subscriberId = await loadSubscriberId(options.subscriberFile);
  const ready = Deferred.makeUnsafe<BridgeReady, Error>();

  const fiber = Effect.runFork(
    runBridge(options, subscriberId, ready).pipe(
      Effect.onExit((exit) => {
        if (Exit.isFailure(exit) && !Deferred.isDoneUnsafe(ready)) {
          return Deferred.fail(
            ready,
            new Error(Cause.pretty(exit.cause)),
          ).pipe(Effect.asVoid);
        }

        return Effect.void;
      }),
      Effect.catchCause(() => Effect.void),
    ),
  );

  const started = await Effect.runPromise(Deferred.await(ready));
  let closed = false;

  return {
    port: started.port,
    subscriberId,
    close: async () => {
      if (closed) return;
      closed = true;
      await Effect.runPromise(Fiber.interrupt(fiber));
    },
  };
};
