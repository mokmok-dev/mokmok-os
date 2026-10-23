import { Cause, Effect, Option, Queue, Ref, Schema, Semaphore, Stream } from "effect";
import {
  LogCorrupt,
  type LogFailure,
  LogReadFailed,
  PublishFailed,
  SubscriberLagged,
  type SubscribeError,
} from "./errors.ts";
import { type Event, encodeEventLine, type Payload, parseLog } from "./event.ts";
import type { LogStore } from "./log.ts";

export type From = "beginning" | "live" | { readonly after: number };

export interface Bus {
  readonly head: Effect.Effect<number>;
  readonly publish: (topic: string, payload: Payload) => Effect.Effect<number, PublishFailed>;
  readonly subscribe: (topic: string, from: From) => Stream.Stream<Event, SubscribeError>;
}

export interface BusMachine {
  readonly bus: Bus;
  readonly liveSubscriptions: Effect.Effect<number>;
}

interface Subscriber {
  readonly topic: string;
  readonly queue: Queue.Queue<Event, SubscriberLagged>;
}

interface State {
  readonly head: number;
  readonly subscribers: ReadonlySet<Subscriber>;
}

const starts: Record<"beginning" | "live", (head: number) => Option.Option<number>> = {
  beginning: () => Option.none(),
  live: Option.some,
};

const resolveFrom = (from: From, head: number): Option.Option<number> =>
  typeof from === "string" ? starts[from](head) : Option.some(from.after);

const passes = (after: Option.Option<number>, seq: number): boolean =>
  Option.match(after, {
    onNone: () => true,
    onSome: (at) => seq > at,
  });

const validate = (payload: Payload): Effect.Effect<Payload, PublishFailed> =>
  Option.match(Schema.decodeUnknownOption(Schema.Json)(payload), {
    onNone: () => Effect.fail(new PublishFailed({ cause: payload })),
    onSome: (valid) => Effect.succeed(valid),
  });

export const makeBus = (options: {
  readonly log: LogStore;
  readonly maxLag: number;
}): Effect.Effect<BusMachine, LogCorrupt | LogFailure> =>
  Effect.gen(function* () {
    const bytes = yield* options.log.readAll;
    const recovered = yield* Effect.sync(() => parseLog(bytes)).pipe(
      Effect.catchDefect((cause) => Effect.fail(new LogCorrupt({ cause }))),
    );
    yield* options.log.truncate(recovered.validBytes);

    const state = yield* Ref.make<State>({
      head: recovered.events.length,
      subscribers: new Set(),
    });
    const commit = yield* Semaphore.make(1);

    const head = Ref.get(state).pipe(Effect.map((current) => current.head));
    const liveSubscriptions = Ref.get(state).pipe(
      Effect.map((current) => current.subscribers.size),
    );

    const publish = (topic: string, payload: Payload): Effect.Effect<number, PublishFailed> =>
      validate(payload).pipe(
        Effect.andThen(
          commit.withPermit(
            Effect.gen(function* () {
              const current = yield* Ref.get(state);
              const seq = current.head + 1;
              yield* options.log.append(encodeEventLine({ seq, topic, payload }));
              for (const subscriber of current.subscribers) {
                if (subscriber.topic === topic) {
                  if (!Queue.offerUnsafe(subscriber.queue, { seq, topic, payload })) {
                    Queue.failCauseUnsafe(subscriber.queue, Cause.fail(new SubscriberLagged()));
                  }
                }
              }
              yield* Ref.set(state, { head: seq, subscribers: current.subscribers });
              return seq;
            }).pipe(
              Effect.uninterruptible,
              Effect.mapError((cause) => new PublishFailed({ cause })),
            ),
          ),
        ),
      );

    const subscribe = (topic: string, from: From): Stream.Stream<Event, SubscribeError> =>
      Stream.unwrap(
        Effect.gen(function* () {
          const queue = yield* Queue.bounded<Event, SubscriberLagged>(options.maxLag);
          const subscriber: Subscriber = { topic, queue };
          yield* Effect.addFinalizer(() =>
            commit.withPermit(
              Ref.update(state, (current) => ({
                head: current.head,
                subscribers: new Set(
                  [...current.subscribers].filter((other) => other !== subscriber),
                ),
              })),
            ),
          );
          const headAtStart = yield* commit.withPermit(
            Effect.gen(function* () {
              const current = yield* Ref.get(state);
              yield* Ref.set(state, {
                head: current.head,
                subscribers: new Set(current.subscribers).add(subscriber),
              });
              return current.head;
            }),
          );
          const replayBytes = yield* options.log.readAll.pipe(
            Effect.mapError((cause) => new LogReadFailed({ cause })),
          );
          const events = yield* Effect.sync(() => parseLog(replayBytes).events).pipe(
            Effect.catchDefect((cause) => Effect.fail(new LogReadFailed({ cause }))),
          );
          const after = resolveFrom(from, headAtStart);
          const replay = events.filter(
            (event) =>
              event.topic === topic && event.seq <= headAtStart && passes(after, event.seq),
          );
          return Stream.concat(Stream.fromIterable(replay), Stream.fromQueue(queue));
        }),
      );

    return { bus: { head, publish, subscribe }, liveSubscriptions };
  });
