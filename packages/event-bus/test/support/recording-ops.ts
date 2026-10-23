import { Effect } from "effect";
import { nodeLogOps, type LogOps } from "../../src/log.ts";

export interface Recorder {
  readonly ops: LogOps;
  readonly calls: ReadonlyArray<string>;
  readonly written: ReadonlyArray<Uint8Array>;
  readonly truncated: ReadonlyArray<number>;
}

export const recordingOps = (inner: LogOps = nodeLogOps): Recorder => {
  const calls: Array<string> = [];
  const written: Array<Uint8Array> = [];
  const truncated: Array<number> = [];

  const note = (label: string): Effect.Effect<void> =>
    Effect.sync(() => {
      calls.push(label);
    });

  const ops: LogOps = {
    openAppend: (path) => Effect.andThen(note(`openAppend ${path}`), inner.openAppend(path)),
    write: (handle, bytes) =>
      Effect.andThen(
        note("write"),
        Effect.andThen(
          Effect.sync(() => {
            written.push(bytes);
          }),
          inner.write(handle, bytes),
        ),
      ),
    sync: (handle) => Effect.andThen(note("sync"), inner.sync(handle)),
    close: (handle) => Effect.andThen(note("close"), inner.close(handle)),
    readFile: (path) => Effect.andThen(note(`readFile ${path}`), inner.readFile(path)),
    truncate: (handle, length) =>
      Effect.andThen(
        note(`truncate ${length}`),
        Effect.andThen(
          Effect.sync(() => {
            truncated.push(length);
          }),
          inner.truncate(handle, length),
        ),
      ),
    syncDir: (path) => Effect.andThen(note(`syncDir ${path}`), inner.syncDir(path)),
  };

  return { ops, calls, written, truncated };
};
