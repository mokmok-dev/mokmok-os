import { Effect, type Scope, type Stream } from "effect";
import { type From, makeBus } from "./bus.ts";
import {
  EventBusStartFailed,
  LogReadFailed,
  PublishFailed,
  SubscriberLagged,
  type SubscribeError,
} from "./errors.ts";
import type { Event, Payload } from "./event.ts";
import { makeJsonlLog } from "./log.ts";
import { serve } from "./server.ts";

export interface EventBusOptions {
  readonly socketPath: string;
  readonly logPath: string;
  readonly maxLag: number;
}

export interface EventBus {
  readonly head: Effect.Effect<number>;
  readonly publish: (topic: string, payload: Payload) => Effect.Effect<number, PublishFailed>;
  readonly subscribe: (topic: string, from: From) => Stream.Stream<Event, SubscribeError>;
}

export type { Event, From, Payload, SubscribeError };
export { EventBusStartFailed, PublishFailed, SubscriberLagged, LogReadFailed };

const startFailed = (cause: unknown): EventBusStartFailed => new EventBusStartFailed({ cause });

export const makeEventBus = (
  options: EventBusOptions,
): Effect.Effect<EventBus, EventBusStartFailed, Scope.Scope> =>
  Effect.gen(function* () {
    const log = yield* makeJsonlLog(options.logPath).pipe(Effect.mapError(startFailed));
    const { bus } = yield* makeBus({ log, maxLag: options.maxLag }).pipe(
      Effect.mapError(startFailed),
    );
    yield* serve(bus, { socketPath: options.socketPath, maxLag: options.maxLag });
    return bus;
  });
