import { Effect, Option, Queue, Ref, Scope, Semaphore } from "effect";

import {
  describeRefusal,
  type PublishFrame,
  type PublishedResponse,
} from "@mokmok/web-protocol";

import { connectBus, type BusConnection } from "./bus.ts";
import { PublishError } from "./errors.ts";

export type PublishPayload = PublishFrame["payload"];

export interface Publisher {
  publish(payload: PublishPayload): Effect.Effect<PublishedResponse, PublishError>;
}

/**
 * One lazily opened connection shared by every publish request. The bus starts
 * every connection unopened, so the publisher sends a subscriber-less `open`
 * first. Requests are serialized because the bus answers one request at a time.
 */
export const makePublisher = (
  socketPath: string,
  topic: string,
  connections: Scope.Closeable,
): Effect.Effect<Publisher> =>
  Effect.gen(function* () {
    const permit = yield* Semaphore.make(1);
    const current = yield* Ref.make<Option.Option<BusConnection>>(Option.none());

    const retire = (connection: BusConnection): Effect.Effect<void> =>
      Effect.gen(function* () {
        const removed = yield* Ref.modify(current, (state) => {
          if (Option.isSome(state) && state.value === connection) {
            return [true, Option.none<BusConnection>()] as const;
          }

          return [false, state] as const;
        });

        if (removed) yield* connection.close();
      });

    const acquire = Effect.gen(function* () {
      const existing = yield* Ref.get(current);
      if (Option.isSome(existing)) return existing.value;

      const connection = yield* connectBus(socketPath).pipe(
        Effect.mapError(
          (error) =>
            new PublishError({ detail: error.detail, indeterminate: false }),
        ),
      );
      yield* Scope.addFinalizer(connections, connection.close());

      const opening = yield* Effect.gen(function* () {
        yield* connection.send({ op: "open", topic, subscriber: null });
        return yield* Queue.take(connection.responses);
      }).pipe(
        Effect.mapError(
          (error) =>
            new PublishError({ detail: error.detail, indeterminate: false }),
        ),
      );

      if (opening.op === "refused") {
        yield* connection.close();
        return yield* new PublishError({
          detail: describeRefusal(opening.reason),
          indeterminate: false,
        });
      }
      if (opening.op !== "opened") {
        yield* connection.close();
        return yield* new PublishError({
          detail: `the bus answered a publisher open with "${opening.op}"`,
          indeterminate: false,
        });
      }

      yield* Ref.set(current, Option.some(connection));
      return connection;
    });

    const exchange = (
      connection: BusConnection,
      payload: PublishPayload,
    ): Effect.Effect<PublishedResponse, PublishError> =>
      Effect.gen(function* () {
        yield* connection.send({ op: "publish", payload }).pipe(
          Effect.mapError(
            (error) =>
              new PublishError({ detail: error.detail, indeterminate: true }),
          ),
        );
        const response = yield* Queue.take(connection.responses).pipe(
          Effect.mapError(
            (error) =>
              new PublishError({ detail: error.detail, indeterminate: true }),
          ),
        );

        if (response.op === "published") return response;
        if (response.op === "refused") {
          return yield* new PublishError({
            detail: describeRefusal(response.reason),
            indeterminate: false,
          });
        }

        return yield* new PublishError({
          detail: `the bus answered a publish with "${response.op}"`,
          indeterminate: true,
        });
      }).pipe(Effect.tapError(() => retire(connection)));

    const publish = (payload: PublishPayload) =>
      permit.withPermit(
        Effect.gen(function* () {
          const connection = yield* acquire;

          return yield* exchange(connection, payload);
        }),
      );

    return { publish };
  });
