import { Deferred, Effect, HashMap, Option, PubSub, Ref, Stream } from "effect";
import type { Json } from "effect/Schema";

import {
  encodeBridgeClientFrame,
  parseBridgeServerFrame,
  type AckFrame,
  type BridgeClientFrame,
  type BridgeServerFrame,
  type PublishedFrame,
  type PublishFailedFrame,
} from "@mokmok/web-protocol";

export interface PublishSucceeded {
  readonly ok: true;
  readonly requestId: string;
  readonly published: PublishedFrame;
}

export interface PublishFailedOutcome {
  readonly ok: false;
  readonly requestId: string;
  readonly failure: PublishFailedFrame;
}

export type PublishOutcome = PublishSucceeded | PublishFailedOutcome;

export interface BridgeClient {
  /** Decoded server frames, including the synthetic status frames emitted around reconnects. */
  readonly frames: Stream.Stream<BridgeServerFrame>;
  readonly connect: Effect.Effect<void>;
  readonly publish: (requestId: string, payload: Json) => Effect.Effect<PublishOutcome>;
  readonly ack: (frame: AckFrame) => Effect.Effect<boolean>;
}

interface PendingPublish {
  readonly requestId: string;
  readonly deferred: Deferred.Deferred<PublishOutcome>;
}

interface SocketFailure {
  readonly detail: string;
}

const SOCKET_OPEN = 1;
const SOCKET_CLOSED = 3;
const PUBLISH_TIMEOUT = "20 seconds";
const BACKOFF_BASE_MS = 500;
const BACKOFF_CAP_MS = 10_000;
const STABLE_CONNECTION_MS = 5_000;

const failureFrame = (
  requestId: string,
  detail: string,
  indeterminate: boolean,
): PublishFailedFrame => ({
  op: "publish_failed",
  request_id: requestId,
  detail,
  indeterminate,
});

const failure = (requestId: string, detail: string, indeterminate: boolean): PublishFailedOutcome => ({
  ok: false,
  requestId,
  failure: failureFrame(requestId, detail, indeterminate),
});

const openSocket = (url: string): Effect.Effect<WebSocket, SocketFailure> =>
  Effect.callback<WebSocket, SocketFailure>((resume) => {
    const socket = new WebSocket(url);
    const detach = (): void => {
      socket.removeEventListener("open", onOpen);
      socket.removeEventListener("error", onError);
    };
    const onOpen = (): void => {
      detach();
      resume(Effect.succeed(socket));
    };
    const onError = (): void => {
      detach();
      resume(Effect.fail({ detail: "The browser could not open the WebSocket" }));
    };
    socket.addEventListener("open", onOpen);
    socket.addEventListener("error", onError);

    return Effect.sync(() => {
      detach();
      if (socket.readyState < SOCKET_OPEN + 1) socket.close();
    });
  });

const waitForClose = (socket: WebSocket): Effect.Effect<void> =>
  Effect.callback<void>((resume) => {
    if (socket.readyState === SOCKET_CLOSED) {
      resume(Effect.void);
      return;
    }
    const detach = (): void => {
      socket.removeEventListener("close", onClose);
      socket.removeEventListener("error", onError);
    };
    const onClose = (): void => {
      detach();
      resume(Effect.void);
    };
    const onError = (): void => {
      detach();
      resume(Effect.void);
    };
    socket.addEventListener("close", onClose);
    socket.addEventListener("error", onError);

    return Effect.sync(detach);
  });

const jittered = (delayMs: number): number => Math.round(delayMs * (0.8 + Math.random() * 0.4));

/**
 * A scoped WebSocket client. One `connect` run owns exactly one browser socket at a time and
 * reconnects with capped exponential backoff until interrupted.
 */
export const makeBridgeClient = (url: string): Effect.Effect<BridgeClient> =>
  Effect.gen(function* () {
    const inbound = yield* PubSub.unbounded<BridgeServerFrame>();
    const pending = yield* Ref.make(HashMap.empty<string, PendingPublish>());
    const socketRef = yield* Ref.make<WebSocket | null>(null);

    const emit = (frame: BridgeServerFrame): Effect.Effect<boolean> => PubSub.publish(inbound, frame);

    const settle = (requestId: string, outcome: PublishOutcome): Effect.Effect<void> =>
      Effect.gen(function* () {
        const entries = yield* Ref.get(pending);
        const entry = HashMap.get(entries, requestId);
        if (Option.isNone(entry)) return;
        yield* Ref.update(pending, HashMap.remove(requestId));
        yield* Deferred.succeed(entry.value.deferred, outcome);
      });

    const handleFrame = (frame: BridgeServerFrame): Effect.Effect<void> =>
      Effect.gen(function* () {
        if (frame.op === "published") {
          yield* settle(frame.request_id, {
            ok: true,
            requestId: frame.request_id,
            published: frame,
          });
        } else if (frame.op === "publish_failed") {
          yield* settle(frame.request_id, {
            ok: false,
            requestId: frame.request_id,
            failure: frame,
          });
        }
        yield* emit(frame);
      });

    /** Client-side failures are emitted as synthetic frames so the publish log records them too. */
    const failPublish = (
      requestId: string,
      detail: string,
      indeterminate: boolean,
    ): Effect.Effect<PublishOutcome> =>
      Effect.gen(function* () {
        const outcome = failure(requestId, detail, indeterminate);
        yield* emit(outcome.failure);
        return outcome;
      });

    const drainPending = (detail: string): Effect.Effect<void> =>
      Effect.gen(function* () {
        const entries = yield* Ref.get(pending);
        yield* Ref.set(pending, HashMap.empty());
        yield* Effect.forEach(HashMap.values(entries), (entry) =>
          Effect.gen(function* () {
            const outcome = failure(entry.requestId, detail, true);
            yield* emit(outcome.failure);
            yield* Deferred.succeed(entry.deferred, outcome);
          }),
        );
      });

    const send = (frame: BridgeClientFrame): Effect.Effect<boolean> =>
      Effect.gen(function* () {
        const socket = yield* Ref.get(socketRef);
        if (socket === null || socket.readyState !== SOCKET_OPEN) return false;
        return yield* Effect.sync(() => {
          try {
            socket.send(encodeBridgeClientFrame(frame));
            return true;
          } catch {
            return false;
          }
        });
      });

    const publish = (requestId: string, payload: Json): Effect.Effect<PublishOutcome> =>
      Effect.gen(function* () {
        const socket = yield* Ref.get(socketRef);
        if (socket === null || socket.readyState !== SOCKET_OPEN) {
          return yield* failPublish(
            requestId,
            "The console is not connected to the bridge",
            false,
          );
        }

        const deferred = yield* Deferred.make<PublishOutcome>();
        yield* Ref.update(pending, HashMap.set(requestId, { requestId, deferred }));
        const sent = yield* send({ op: "publish", request_id: requestId, payload });
        if (!sent) {
          yield* Ref.update(pending, HashMap.remove(requestId));
          return yield* failPublish(requestId, "The publish frame could not be sent", false);
        }

        const answered = yield* Effect.timeoutOption(Deferred.await(deferred), PUBLISH_TIMEOUT);
        if (Option.isSome(answered)) return answered.value;
        yield* Ref.update(pending, HashMap.remove(requestId));
        return yield* failPublish(
          requestId,
          "The bridge did not confirm the publish before the console timed out",
          true,
        );
      });

    const connectOnce = Effect.scoped(
      Effect.gen(function* () {
        const socket = yield* Effect.acquireRelease(openSocket(url), (acquired) =>
          Effect.gen(function* () {
            yield* Ref.set(socketRef, null);
            yield* Effect.sync(() => {
              if (acquired.readyState < SOCKET_CLOSED) acquired.close();
            });
          }),
        );
        yield* Ref.set(socketRef, socket);

        const onMessage = (event: MessageEvent): void => {
          if (typeof event.data !== "string") return;
          const frame = parseBridgeServerFrame(event.data);
          if (frame === null) return;
          void Effect.runFork(handleFrame(frame));
        };
        socket.addEventListener("message", onMessage);
        yield* Effect.addFinalizer(() => Effect.sync(() => socket.removeEventListener("message", onMessage)));

        yield* waitForClose(socket);
      }),
    );

    const connect = Effect.gen(function* () {
      let backoffMs = BACKOFF_BASE_MS;
      while (true) {
        yield* emit({ op: "status", connection: "connecting", detail: null });
        const startedAt = Date.now();
        const result = yield* Effect.exit(connectOnce);
        const livedMs = Date.now() - startedAt;

        yield* drainPending("The bridge connection closed before the publish was confirmed");
        yield* emit({
          op: "status",
          connection: "offline",
          detail:
            result._tag === "Failure"
              ? "The browser could not open the WebSocket to the bridge"
              : "The connection to the bridge closed",
        });

        if (livedMs >= STABLE_CONNECTION_MS) backoffMs = BACKOFF_BASE_MS;
        yield* Effect.sleep(jittered(backoffMs));
        backoffMs = Math.min(backoffMs * 2, BACKOFF_CAP_MS);
      }
    });

    return {
      frames: Stream.fromPubSub(inbound),
      connect,
      publish,
      ack: (frame: AckFrame) => send(frame),
    } satisfies BridgeClient;
  });
