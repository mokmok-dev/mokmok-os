import type { FileHandle } from "node:fs/promises";
import { open, readFile } from "node:fs/promises";
import { dirname } from "node:path";
import { Effect, type Scope } from "effect";
import { LogFailure } from "./errors.ts";

export interface LogOps {
  readonly openAppend: (path: string) => Effect.Effect<FileHandle, LogFailure>;
  readonly write: (handle: FileHandle, bytes: Uint8Array) => Effect.Effect<void, LogFailure>;
  readonly sync: (handle: FileHandle) => Effect.Effect<void, LogFailure>;
  readonly close: (handle: FileHandle) => Effect.Effect<void, LogFailure>;
  readonly readFile: (path: string) => Effect.Effect<Uint8Array, LogFailure>;
  readonly truncate: (handle: FileHandle, length: number) => Effect.Effect<void, LogFailure>;
  readonly syncDir: (path: string) => Effect.Effect<void, LogFailure>;
}

export interface LogStore {
  readonly append: (line: Uint8Array) => Effect.Effect<void, LogFailure>;
  readonly readAll: Effect.Effect<Uint8Array, LogFailure>;
  readonly truncate: (validBytes: number) => Effect.Effect<void, LogFailure>;
}

const fail = (cause: unknown): LogFailure => new LogFailure({ cause });

export const nodeLogOps: LogOps = {
  openAppend: (path) =>
    Effect.tryPromise({
      try: () => open(path, "a"),
      catch: fail,
    }),
  write: (handle, bytes) =>
    Effect.tryPromise({
      try: async () => {
        await handle.write(bytes);
      },
      catch: fail,
    }),
  sync: (handle) =>
    Effect.tryPromise({
      try: async () => {
        await handle.sync();
      },
      catch: fail,
    }),
  close: (handle) =>
    Effect.tryPromise({
      try: async () => {
        await handle.close();
      },
      catch: fail,
    }),
  readFile: (path) =>
    Effect.tryPromise({
      try: () => readFile(path),
      catch: fail,
    }),
  truncate: (handle, length) =>
    Effect.tryPromise({
      try: async () => {
        await handle.truncate(length);
      },
      catch: fail,
    }),
  syncDir: (path) =>
    Effect.tryPromise({
      try: async () => {
        const dir = await open(path, "r");
        await dir.sync();
        await dir.close();
      },
      catch: fail,
    }),
};

export const makeJsonlLog = (
  path: string,
  ops: LogOps = nodeLogOps,
): Effect.Effect<LogStore, LogFailure, Scope.Scope> =>
  Effect.gen(function* () {
    const handle = yield* Effect.acquireRelease(ops.openAppend(path), (acquired) =>
      ops.close(acquired).pipe(Effect.ignore),
    );
    yield* ops.sync(handle);
    yield* ops.syncDir(dirname(path));
    return {
      append: (line) => Effect.andThen(ops.write(handle, line), ops.sync(handle)),
      readAll: ops.readFile(path),
      truncate: (validBytes) => ops.truncate(handle, validBytes),
    };
  });
