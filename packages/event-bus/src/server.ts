import { chmod, rm } from "node:fs/promises";
import * as http from "node:http";
import { Cause, Effect, Queue, Ref, type Scope, Stream } from "effect";
import { WebSocketServer, type WebSocket as WsSocket } from "ws";
import type { Bus } from "./bus.ts";
import { EventBusStartFailed } from "./errors.ts";
import { decodeClientFrame, encodeServerFrame, type ServerFrame } from "./wire.ts";

export interface Peer {
  readonly messages: Stream.Stream<string>;
  readonly send: (text: string) => Effect.Effect<void>;
  readonly close: (code: number) => Effect.Effect<void>;
}

export interface PeerSocket {
  readonly on: (event: string, listener: (...args: ReadonlyArray<unknown>) => void) => void;
  readonly send: (text: string, callback: (error?: Error) => void) => void;
  readonly close: (code: number) => void;
}

const attempt = <A>(thunk: () => Promise<A>): Effect.Effect<A, EventBusStartFailed> =>
  Effect.tryPromise({
    try: thunk,
    catch: (cause) => new EventBusStartFailed({ cause }),
  });

const removeSocket = (path: string): Effect.Effect<void, EventBusStartFailed> =>
  attempt(() => rm(path, { force: true }));

const listen = (server: http.Server, path: string): Effect.Effect<void, EventBusStartFailed> =>
  attempt(
    () =>
      new Promise<void>((resolve, reject) => {
        server.on("error", reject);
        server.listen(path, resolve);
      }),
  );

const awaitWebSocketServerClose = (server: WebSocketServer): Effect.Effect<void> =>
  Effect.promise(
    () =>
      new Promise<void>((resolve) => {
        server.close(() => resolve());
      }),
  );

const awaitHttpServerClose = (server: http.Server): Effect.Effect<void> =>
  Effect.promise(
    () =>
      new Promise<void>((resolve) => {
        server.close(() => resolve());
      }),
  );

export const wsPeer = (
  socket: PeerSocket,
  maxLag: number,
): Effect.Effect<Peer, never, Scope.Scope> =>
  Effect.gen(function* () {
    const inbound = yield* Queue.bounded<string, Cause.Done>(maxLag);
    yield* Effect.sync(() => {
      socket.on("message", (data) => {
        if (!Queue.offerUnsafe(inbound, String(data))) {
          socket.close(1013);
        }
      });
      socket.on("close", () => {
        Queue.endUnsafe(inbound);
      });
      socket.on("error", () => {});
    });
    return {
      messages: Stream.fromQueue(inbound),
      send: (text) =>
        Effect.promise(
          () =>
            new Promise<void>((resolve) => {
              socket.send(text, () => resolve());
            }),
        ),
      close: (code) =>
        Effect.sync(() => {
          socket.close(code);
        }),
    };
  });

export const handleConnection = (bus: Bus, peer: Peer, maxLag: number): Effect.Effect<void> =>
  Effect.scoped(
    Effect.gen(function* () {
      const outbound = yield* Queue.bounded<ServerFrame>(maxLag);
      yield* Effect.forkScoped(
        Stream.runForEach(Stream.fromQueue(outbound), (frame) =>
          peer.send(encodeServerFrame(frame)),
        ),
      );
      yield* Queue.offer(outbound, { op: "hello", head: yield* bus.head });
      const subscribed = yield* Ref.make(false);
      yield* Stream.runForEach(peer.messages, (text) =>
        Effect.gen(function* () {
          const frame = decodeClientFrame(text);
          if (frame === undefined) {
            return yield* peer.close(1002);
          }
          if (frame.op === "publish") {
            return yield* bus.publish(frame.topic, frame.payload).pipe(
              Effect.flatMap((seq) =>
                Queue.offer(outbound, { op: "published", req: frame.req, seq }),
              ),
              Effect.catchTag("PublishFailed", () =>
                Queue.offer(outbound, { op: "error", req: frame.req, code: "write_failed" }),
              ),
            );
          }
          const already = yield* Ref.getAndSet(subscribed, true);
          if (already) {
            return yield* peer.close(1002);
          }
          yield* Effect.forkScoped(
            bus.subscribe(frame.topic, { after: frame.after }).pipe(
              Stream.runForEach((event) =>
                Queue.offer(outbound, {
                  op: "event",
                  seq: event.seq,
                  topic: event.topic,
                  payload: event.payload,
                }),
              ),
              Effect.catchTag("SubscriberLagged", () => peer.close(1013)),
              Effect.catchTag("LogReadFailed", () => peer.close(1011)),
            ),
          );
        }),
      );
      yield* peer.close(1000);
    }),
  );

export const serve = (
  bus: Bus,
  options: { readonly socketPath: string; readonly maxLag: number },
): Effect.Effect<void, EventBusStartFailed, Scope.Scope> =>
  Effect.gen(function* () {
    const server = http.createServer();
    const sockets = new WebSocketServer({ noServer: true });
    const accepted = yield* Queue.unbounded<WsSocket>();
    server.on("upgrade", (request, socket, head) => {
      sockets.handleUpgrade(request, socket, head, (client) => {
        sockets.emit("connection", client, request);
      });
    });
    sockets.on("connection", (socket) => {
      Queue.offerUnsafe(accepted, socket);
    });
    yield* Effect.acquireRelease(
      Effect.gen(function* () {
        yield* removeSocket(options.socketPath);
        yield* listen(server, options.socketPath);
        yield* attempt(() => chmod(options.socketPath, 0o600));
        return server;
      }),
      () =>
        Effect.gen(function* () {
          yield* Effect.sync(() => {
            for (const client of sockets.clients) {
              client.terminate();
            }
          });
          yield* awaitWebSocketServerClose(sockets);
          yield* awaitHttpServerClose(server);
          yield* removeSocket(options.socketPath).pipe(Effect.ignore);
        }),
    );
    yield* Effect.forkScoped(
      Stream.runForEach(Stream.fromQueue(accepted), (socket) =>
        Effect.forkScoped(
          Effect.scoped(
            Effect.gen(function* () {
              const peer = yield* wsPeer(socket, options.maxLag);
              yield* handleConnection(bus, peer, options.maxLag);
            }),
          ),
        ),
      ),
    );
  });
