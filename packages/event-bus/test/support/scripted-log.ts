import { Deferred, Effect } from "effect";
import { LogFailure } from "../../src/errors.ts";
import { parseLog } from "../../src/event.ts";
import type { LogStore } from "../../src/log.ts";

export interface ScriptedLogOptions {
  readonly initial?: Uint8Array;
  readonly failAppends?: number;
}

export interface ScriptedLog {
  readonly store: LogStore;
  readonly bytes: () => Uint8Array;
  readonly lines: () => ReadonlyArray<string>;
  readonly events: () => ReturnType<typeof parseLog>["events"];
  readonly truncations: () => ReadonlyArray<number>;
  readonly appends: () => number;
  readonly reads: () => number;
  readonly setBytes: (bytes: Uint8Array) => void;
  readonly failNextRead: () => void;
  readonly armAppendGate: (gate: Deferred.Deferred<void>, entered: Deferred.Deferred<void>) => void;
  readonly armReadGate: (
    gate: Deferred.Deferred<void>,
    entered: Deferred.Deferred<void>,
    done?: Deferred.Deferred<void>,
  ) => void;
}

export const scriptedLog = (options: ScriptedLogOptions = {}): ScriptedLog => {
  let bytes = options.initial ?? new Uint8Array(0);
  let appends = 0;
  let reads = 0;
  let failures = options.failAppends ?? 0;
  let readFailure = false;
  const truncations: Array<number> = [];
  let appendGate: Deferred.Deferred<void> | undefined;
  let appendEntered: Deferred.Deferred<void> | undefined;
  let readGate: Deferred.Deferred<void> | undefined;
  let readEntered: Deferred.Deferred<void> | undefined;
  let readDone: Deferred.Deferred<void> | undefined;

  const pause = (
    gate: Deferred.Deferred<void> | undefined,
    entered: Deferred.Deferred<void> | undefined,
  ): Effect.Effect<void> =>
    Effect.andThen(
      entered === undefined ? Effect.void : Deferred.succeed(entered, void 0),
      gate === undefined ? Effect.void : Deferred.await(gate),
    );

  const signal = (done: Deferred.Deferred<void> | undefined): Effect.Effect<void> =>
    done === undefined ? Effect.void : Deferred.succeed(done, void 0);

  const store: LogStore = {
    append: (line) =>
      Effect.suspend(() => {
        if (failures > 0) {
          failures -= 1;
          return Effect.fail(new LogFailure({ cause: "scripted append failure" }));
        }
        appends += 1;
        const next = new Uint8Array(bytes.length + line.length);
        next.set(bytes);
        next.set(line, bytes.length);
        bytes = next;
        return pause(appendGate, appendEntered);
      }),
    readAll: Effect.suspend(() => {
      reads += 1;
      if (readFailure) {
        readFailure = false;
        return Effect.fail(new LogFailure({ cause: "scripted read failure" }));
      }
      return Effect.andThen(
        pause(readGate, readEntered),
        Effect.andThen(
          Effect.sync(() => bytes),
          (snapshot) => Effect.andThen(signal(readDone), Effect.succeed(snapshot)),
        ),
      );
    }),
    truncate: (validBytes) =>
      Effect.sync(() => {
        truncations.push(validBytes);
        bytes = bytes.subarray(0, validBytes);
      }),
  };

  return {
    store,
    bytes: () => bytes,
    lines: () =>
      new TextDecoder()
        .decode(bytes)
        .split("\n")
        .filter((line) => line.length > 0),
    events: () => parseLog(bytes).events,
    truncations: () => truncations,
    appends: () => appends,
    reads: () => reads,
    setBytes: (next) => {
      bytes = next;
    },
    failNextRead: () => {
      readFailure = true;
    },
    armAppendGate: (gate, entered) => {
      appendGate = gate;
      appendEntered = entered;
    },
    armReadGate: (gate, entered, done) => {
      readGate = gate;
      readEntered = entered;
      readDone = done;
    },
  };
};
