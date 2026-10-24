import { open, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { Effect } from "effect";
import { expect, it } from "vitest";
import { LogFailure } from "../src/errors.ts";
import { encodeEventLine, parseLog } from "../src/event.ts";
import { makeJsonlLog, nodeLogOps } from "../src/log.ts";
import { run, tempDir } from "./support/helpers.ts";
import { recordingOps } from "./support/recording-ops.ts";

const utf8 = (text: string): Uint8Array => new TextEncoder().encode(text);
const text = (bytes: Uint8Array): string => new TextDecoder().decode(bytes);

const codeOf = (error: LogFailure): string | undefined =>
  (error.cause as NodeJS.ErrnoException).code;

const logPath = async (): Promise<string> => join(await tempDir(), "bus.jsonl");

it("log store opens for append, fsyncs the file, fsyncs the directory, and closes on scope release", async () => {
  const path = await logPath();
  const recorder = recordingOps();
  await run(
    Effect.scoped(
      Effect.gen(function* () {
        yield* makeJsonlLog(path, recorder.ops);
      }),
    ),
  );
  const dir = path.slice(0, path.lastIndexOf("/"));
  expect(recorder.calls).toEqual([`openAppend ${path}`, "sync", `syncDir ${dir}`, "close"]);
});

it("log store appends the exact bytes and syncs before resolving", async () => {
  const path = await logPath();
  const recorder = recordingOps();
  const line = encodeEventLine({ seq: 1, topic: "t", payload: { a: 1 } });
  await run(
    Effect.scoped(
      Effect.gen(function* () {
        const log = yield* makeJsonlLog(path, recorder.ops);
        yield* log.append(line);
        expect(recorder.calls.slice(-2)).toEqual(["write", "sync"]);
        expect(recorder.written).toEqual([line]);
      }),
    ),
  );
  expect(await readFile(path)).toEqual(Buffer.from(line));
});

it("log store keeps the bytes already on disk when it opens an existing log", async () => {
  const path = await logPath();
  await writeFile(path, "existing\n");
  const bytes = await run(
    Effect.scoped(
      Effect.gen(function* () {
        const log = yield* makeJsonlLog(path);
        yield* log.append(utf8("next\n"));
        return yield* log.readAll;
      }),
    ),
  );
  expect(text(bytes)).toBe("existing\nnext\n");
});

it("log store reads the whole file back", async () => {
  const path = await logPath();
  await writeFile(path, "abc\n");
  const bytes = await run(
    Effect.scoped(
      Effect.gen(function* () {
        const log = yield* makeJsonlLog(path);
        return yield* log.readAll;
      }),
    ),
  );
  expect(text(bytes)).toBe("abc\n");
});

it("log store truncates to the requested byte length", async () => {
  const path = await logPath();
  await writeFile(path, "abcdef\n");
  const recorder = recordingOps();
  await run(
    Effect.scoped(
      Effect.gen(function* () {
        const log = yield* makeJsonlLog(path, recorder.ops);
        yield* log.truncate(3);
      }),
    ),
  );
  expect(recorder.truncated).toEqual([3]);
  expect(text(await readFile(path))).toBe("abc");
});

it("log store leaves a line readable after the handle is closed and the log reopened", async () => {
  const path = await logPath();
  const line = encodeEventLine({ seq: 1, topic: "t", payload: "value" });
  await run(
    Effect.scoped(
      Effect.gen(function* () {
        const log = yield* makeJsonlLog(path);
        yield* log.append(line);
      }),
    ),
  );
  const bytes = await run(
    Effect.scoped(
      Effect.gen(function* () {
        const log = yield* makeJsonlLog(path);
        return yield* log.readAll;
      }),
    ),
  );
  expect(parseLog(bytes).events).toEqual([{ seq: 1, topic: "t", payload: "value" }]);
});

it("log ops reports a missing log file as LogFailure with the underlying error", async () => {
  const path = await logPath();
  const error = await run(Effect.flip(nodeLogOps.readFile(path)));
  expect(error._tag).toBe("LogFailure");
  expect(codeOf(error)).toBe("ENOENT");
});

it("log ops reports an open in a missing directory as LogFailure", async () => {
  const path = join(await tempDir(), "missing", "bus.jsonl");
  const error = await run(Effect.flip(nodeLogOps.openAppend(path)));
  expect(error._tag).toBe("LogFailure");
  expect(codeOf(error)).toBe("ENOENT");
});

it("log ops reports a directory fsync of a missing path as LogFailure", async () => {
  const path = join(await tempDir(), "missing");
  const error = await run(Effect.flip(nodeLogOps.syncDir(path)));
  expect(error._tag).toBe("LogFailure");
  expect(codeOf(error)).toBe("ENOENT");
});

it("log ops fsyncs a directory that exists", async () => {
  const dir = await tempDir();
  await run(nodeLogOps.syncDir(dir));
});

it("log ops reports a write to a closed handle as LogFailure", async () => {
  const path = await logPath();
  const handle = await open(path, "a");
  await run(nodeLogOps.close(handle));
  const error = await run(Effect.flip(nodeLogOps.write(handle, utf8("x"))));
  expect(error._tag).toBe("LogFailure");
  expect(codeOf(error)).toBe("EBADF");
});

it("log ops reports a sync of a closed handle as LogFailure", async () => {
  const path = await logPath();
  const handle = await open(path, "a");
  await run(nodeLogOps.close(handle));
  const error = await run(Effect.flip(nodeLogOps.sync(handle)));
  expect(error._tag).toBe("LogFailure");
  expect(codeOf(error)).toBe("EBADF");
});

it("log ops reports a truncate of a closed handle as LogFailure", async () => {
  const path = await logPath();
  const handle = await open(path, "a");
  await run(nodeLogOps.close(handle));
  const error = await run(Effect.flip(nodeLogOps.truncate(handle, 0)));
  expect(error._tag).toBe("LogFailure");
  expect(codeOf(error)).toBe("EBADF");
});

it("log ops closes a handle that is still open", async () => {
  const path = await logPath();
  const handle = await open(path, "a");
  await run(nodeLogOps.close(handle));
  const error = await run(Effect.flip(nodeLogOps.sync(handle)));
  expect(error._tag).toBe("LogFailure");
});
