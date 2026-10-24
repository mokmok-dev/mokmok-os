import { randomUUID } from "node:crypto";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Effect, Exit } from "effect";

export const run = <A, E>(effect: Effect.Effect<A, E>): Promise<A> =>
  Effect.runPromise(effect.pipe(Effect.timeout("5 seconds")));

export const runExit = <A, E>(effect: Effect.Effect<A, E>): Promise<Exit.Exit<A, E | unknown>> =>
  Effect.runPromise(Effect.exit(effect.pipe(Effect.timeout("5 seconds"))));

export const waitFor = (predicate: () => boolean): Effect.Effect<void> =>
  Effect.suspend(() =>
    predicate() ? Effect.void : Effect.sleep("2 millis").pipe(Effect.andThen(waitFor(predicate))),
  );

export const tempDir = (): Promise<string> => mkdtemp(join(tmpdir(), "mokmok-bus-"));

export const tempSocket = (): string => join("/tmp", `mk-${randomUUID().slice(0, 8)}.sock`);
