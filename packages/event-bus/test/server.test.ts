import { mkdir, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { Effect, Exit, Fiber, Option, Stream } from "effect";
import { expect, it } from "vitest";
import { type Bus, makeBus } from "../src/bus.ts";
import {
  type EventBusOptions,
  EventBusStartFailed,
  LogReadFailed,
  SubscriberLagged,
  makeEventBus,
} from "../src/index.ts";
import type { Peer } from "../src/server.ts";
import { handleConnection, wsPeer } from "../src/server.ts";
import { connect, takeOp } from "./support/client.ts";
import { fakePeer } from "./support/fake-peer.ts";
import { fakeSocket } from "./support/fake-socket.ts";
import { run, runExit, tempDir, tempSocket, waitFor } from "./support/helpers.ts";
import { scriptedLog, type ScriptedLog } from "./support/scripted-log.ts";

const makeTestBus = (log: ScriptedLog, maxLag = 16) =>
  makeBus({ log: log.store, maxLag }).pipe(Effect.map(({ bus }) => bus));

const fakeBus = (options: {
  readonly head?: number;
  readonly publish?: Bus["publish"];
  readonly subscribe?: Bus["subscribe"];
}): Bus => ({
  head: Effect.succeed(options.head ?? 0),
  publish: options.publish ?? (() => Effect.succeed(1)),
  subscribe: options.subscribe ?? (() => Stream.empty),
});

const drive = (bus: Bus, peer: Peer, maxLag = 16) =>
  Effect.gen(function* () {
    const fiber = yield* Effect.forkChild(handleConnection(bus, peer, maxLag));
    return fiber;
  });

it("connection greets a new connection with the current head", async () => {
  const frames = await run(
    Effect.gen(function* () {
      const log = scriptedLog();
      const bus = yield* makeTestBus(log);
      yield* bus.publish("t", 1);
      const fake = fakePeer();
      const fiber = yield* drive(bus, fake.peer);
      yield* waitFor(() => fake.frames.length === 1);
      const seen = [...fake.frames];
      yield* Fiber.interrupt(fiber);
      return seen;
    }),
  );
  expect(frames).toEqual([{ op: "hello", head: 1 }]);
});

it("connection acknowledges a publish with its request id and sequence", async () => {
  const result = await run(
    Effect.gen(function* () {
      const log = scriptedLog();
      const bus = yield* makeTestBus(log);
      const fake = fakePeer();
      const fiber = yield* drive(bus, fake.peer);
      yield* waitFor(() => fake.frames.length === 1);
      yield* fake.push('{"op":"publish","req":7,"topic":"t","payload":{"a":1}}');
      yield* waitFor(() => fake.frames.length === 2);
      const frames = [...fake.frames];
      const head = yield* bus.head;
      yield* Fiber.interrupt(fiber);
      return { frames, head };
    }),
  );
  expect(result.frames[1]).toEqual({ op: "published", req: 7, seq: 1 });
  expect(result.head).toBe(1);
});

it("connection answers a failed publish with error/write_failed and keeps the connection", async () => {
  const result = await run(
    Effect.gen(function* () {
      const log = scriptedLog({ failAppends: 1 });
      const bus = yield* makeTestBus(log);
      const fake = fakePeer();
      const fiber = yield* drive(bus, fake.peer);
      yield* waitFor(() => fake.frames.length === 1);
      yield* fake.push('{"op":"publish","req":9,"topic":"t","payload":1}');
      yield* waitFor(() => fake.frames.length === 2);
      const failed = [...fake.frames];
      const headAfterFailure = yield* bus.head;
      yield* fake.push('{"op":"publish","req":10,"topic":"t","payload":2}');
      yield* waitFor(() => fake.frames.length === 3);
      const retried = [...fake.frames];
      yield* Fiber.interrupt(fiber);
      return { failed, retried, headAfterFailure, closes: [...fake.closes] };
    }),
  );
  expect(result.failed[1]).toEqual({ op: "error", req: 9, code: "write_failed" });
  expect(result.headAfterFailure).toBe(0);
  expect(result.retried[2]).toEqual({ op: "published", req: 10, seq: 1 });
  expect(result.closes).toEqual([]);
});

it("connection replays the durable prefix and then follows live events", async () => {
  const result = await run(
    Effect.gen(function* () {
      const log = scriptedLog();
      const bus = yield* makeTestBus(log);
      yield* bus.publish("t", "one");
      yield* bus.publish("t", "two");
      const fake = fakePeer();
      const fiber = yield* drive(bus, fake.peer);
      yield* waitFor(() => fake.frames.length === 1);
      yield* fake.push('{"op":"subscribe","req":8,"topic":"t","after":0}');
      yield* waitFor(() => fake.frames.length === 3);
      const replayed = [...fake.frames];
      yield* bus.publish("t", "three");
      yield* waitFor(() => fake.frames.length === 4);
      const live = [...fake.frames];
      yield* Fiber.interrupt(fiber);
      return { replayed, live };
    }),
  );
  expect(result.replayed.slice(1)).toEqual([
    { op: "event", seq: 1, topic: "t", payload: "one" },
    { op: "event", seq: 2, topic: "t", payload: "two" },
  ]);
  expect(result.live[3]).toEqual({ op: "event", seq: 3, topic: "t", payload: "three" });
});

it("connection closes a connection that subscribes twice with 1002", async () => {
  const closes = await run(
    Effect.gen(function* () {
      const log = scriptedLog();
      const bus = yield* makeTestBus(log);
      const fake = fakePeer();
      const fiber = yield* drive(bus, fake.peer);
      yield* waitFor(() => fake.frames.length === 1);
      yield* fake.push('{"op":"subscribe","req":8,"topic":"t","after":0}');
      yield* fake.push('{"op":"subscribe","req":9,"topic":"t","after":0}');
      yield* waitFor(() => fake.closes.length === 1);
      const seen = [...fake.closes];
      yield* Fiber.interrupt(fiber);
      return seen;
    }),
  );
  expect(closes).toEqual([1002]);
});

it("connection closes a connection that sends an undecodable frame with 1002", async () => {
  const closes = await run(
    Effect.gen(function* () {
      const log = scriptedLog();
      const bus = yield* makeTestBus(log);
      const fake = fakePeer();
      const fiber = yield* drive(bus, fake.peer);
      yield* waitFor(() => fake.frames.length === 1);
      yield* fake.push("not a frame");
      yield* waitFor(() => fake.closes.length === 1);
      const seen = [...fake.closes];
      yield* Fiber.interrupt(fiber);
      return seen;
    }),
  );
  expect(closes).toEqual([1002]);
});

it("connection closes a lagging subscription with 1013", async () => {
  const closes = await run(
    Effect.gen(function* () {
      const fake = fakePeer();
      const bus = fakeBus({ subscribe: () => Stream.fail(new SubscriberLagged()) });
      const fiber = yield* drive(bus, fake.peer);
      yield* waitFor(() => fake.frames.length === 1);
      yield* fake.push('{"op":"subscribe","req":8,"topic":"t","after":0}');
      yield* waitFor(() => fake.closes.length === 1);
      const seen = [...fake.closes];
      yield* Fiber.interrupt(fiber);
      return seen;
    }),
  );
  expect(closes).toEqual([1013]);
});

it("connection closes a subscription that cannot read the log with 1011", async () => {
  const closes = await run(
    Effect.gen(function* () {
      const fake = fakePeer();
      const bus = fakeBus({ subscribe: () => Stream.fail(new LogReadFailed({ cause: "disk" })) });
      const fiber = yield* drive(bus, fake.peer);
      yield* waitFor(() => fake.frames.length === 1);
      yield* fake.push('{"op":"subscribe","req":8,"topic":"t","after":0}');
      yield* waitFor(() => fake.closes.length === 1);
      const seen = [...fake.closes];
      yield* Fiber.interrupt(fiber);
      return seen;
    }),
  );
  expect(closes).toEqual([1011]);
});

it("connection closes the peer when the inbound stream ends", async () => {
  const closes = await run(
    Effect.gen(function* () {
      const bus = fakeBus({});
      const fake = fakePeer();
      const fiber = yield* drive(bus, fake.peer);
      yield* waitFor(() => fake.frames.length === 1);
      yield* fake.end;
      yield* Fiber.join(fiber);
      return [...fake.closes];
    }),
  );
  expect(closes).toEqual([1000]);
});

it("peer turns an inbound message into an element of the message stream", async () => {
  const text = await run(
    Effect.scoped(
      Effect.gen(function* () {
        const fake = fakeSocket();
        const peer = yield* wsPeer(fake.socket, 16);
        fake.emitEvent("message", Buffer.from("hello"));
        return yield* Stream.runHead(peer.messages);
      }),
    ),
  );
  expect(text).toEqual(Option.some("hello"));
});

it("peer ends the message stream when the socket closes", async () => {
  const messages = await run(
    Effect.scoped(
      Effect.gen(function* () {
        const fake = fakeSocket();
        const peer = yield* wsPeer(fake.socket, 16);
        fake.emitEvent("message", Buffer.from("one"));
        fake.emitEvent("close");
        return yield* Stream.runCollect(peer.messages);
      }),
    ),
  );
  expect(messages).toEqual(["one"]);
});

it("peer survives a socket error event", async () => {
  await run(
    Effect.scoped(
      Effect.gen(function* () {
        const fake = fakeSocket();
        yield* wsPeer(fake.socket, 16);
        fake.emitEvent("error", new Error("boom"));
      }),
    ),
  );
});

it("peer closes the socket with 1013 when inbound messages overflow", async () => {
  const closes = await run(
    Effect.scoped(
      Effect.gen(function* () {
        const fake = fakeSocket();
        yield* wsPeer(fake.socket, 1);
        fake.emitEvent("message", Buffer.from("one"));
        fake.emitEvent("message", Buffer.from("two"));
        return [...fake.closes];
      }),
    ),
  );
  expect(closes).toEqual([1013]);
});

it("peer resolves a send only once the transport acknowledges it", async () => {
  const result = await run(
    Effect.scoped(
      Effect.gen(function* () {
        const fake = fakeSocket();
        const peer = yield* wsPeer(fake.socket, 16);
        const fiber = yield* Effect.forkChild(peer.send("frame"));
        yield* waitFor(() => fake.pending() === 1);
        const beforeAck = fiber.pollUnsafe();
        fake.flush();
        yield* Fiber.join(fiber);
        return { beforeAck, sent: [...fake.sent] };
      }),
    ),
  );
  expect(result.beforeAck).toBeUndefined();
  expect(result.sent).toEqual(["frame"]);
});

it("peer closes the socket with the requested code", async () => {
  const closes = await run(
    Effect.scoped(
      Effect.gen(function* () {
        const fake = fakeSocket();
        const peer = yield* wsPeer(fake.socket, 16);
        yield* peer.close(1002);
        return [...fake.closes];
      }),
    ),
  );
  expect(closes).toEqual([1002]);
});

const options = async (overrides: Partial<EventBusOptions> = {}): Promise<EventBusOptions> => ({
  socketPath: tempSocket(),
  logPath: join(await tempDir(), "bus.jsonl"),
  maxLag: 16,
  ...overrides,
});

const withBus = async (
  busOptions: EventBusOptions,
  body: (bus: Awaited<ReturnType<typeof makeEventBus>>) => Promise<unknown>,
): Promise<void> => {
  await run(
    Effect.gen(function* () {
      const bus = yield* makeEventBus(busOptions);
      yield* Effect.promise(() => body(bus));
    }).pipe(Effect.scoped),
  );
};

it("socket creates a socket only the owner may use", async () => {
  const busOptions = await options();
  let mode = 0;
  await withBus(busOptions, async () => {
    mode = (await stat(busOptions.socketPath)).mode & 0o777;
  });
  expect(mode).toBe(0o600);
});

it("socket removes the socket file when the scope closes", async () => {
  const busOptions = await options();
  await withBus(busOptions, async () => {
    expect((await stat(busOptions.socketPath)).isSocket()).toBe(true);
  });
  await expect(stat(busOptions.socketPath)).rejects.toThrow();
});

it("socket replaces a stale socket file", async () => {
  const busOptions = await options();
  await writeFile(busOptions.socketPath, "stale");
  await withBus(busOptions, async () => {
    expect((await stat(busOptions.socketPath)).isSocket()).toBe(true);
    const client = await connect(busOptions.socketPath);
    await client.take(takeOp("hello"));
    client.stop();
  });
});

it("socket publishes for one client and acknowledges the sequence", async () => {
  const busOptions = await options();
  await withBus(busOptions, async () => {
    const client = await connect(busOptions.socketPath);
    expect(await client.take(takeOp("hello"))).toEqual({ op: "hello", head: 0 });
    client.send({ op: "publish", req: 7, topic: "t", payload: { a: 1 } });
    expect(await client.take(takeOp("published"))).toEqual({ op: "published", req: 7, seq: 1 });
    client.stop();
  });
});

it("socket replays an event to a second client", async () => {
  const busOptions = await options();
  await withBus(busOptions, async () => {
    const publisher = await connect(busOptions.socketPath);
    await publisher.take(takeOp("hello"));
    publisher.send({ op: "publish", req: 1, topic: "t", payload: "value" });
    await publisher.take(takeOp("published"));

    const subscriber = await connect(busOptions.socketPath);
    expect(await subscriber.take(takeOp("hello"))).toEqual({ op: "hello", head: 1 });
    subscriber.send({ op: "subscribe", req: 2, topic: "t", after: 0 });
    expect(await subscriber.take(takeOp("event"))).toEqual({
      op: "event",
      seq: 1,
      topic: "t",
      payload: "value",
    });
    publisher.stop();
    subscriber.stop();
  });
});

it("socket replays the durable prefix to a restarted server on the same log", async () => {
  const busOptions = await options();
  await withBus(busOptions, async () => {
    const client = await connect(busOptions.socketPath);
    await client.take(takeOp("hello"));
    client.send({ op: "publish", req: 1, topic: "t", payload: "kept" });
    await client.take(takeOp("published"));
    client.stop();
  });
  await withBus(busOptions, async () => {
    const client = await connect(busOptions.socketPath);
    expect(await client.take(takeOp("hello"))).toEqual({ op: "hello", head: 1 });
    client.send({ op: "subscribe", req: 2, topic: "t", after: 0 });
    expect(await client.take(takeOp("event"))).toEqual({
      op: "event",
      seq: 1,
      topic: "t",
      payload: "kept",
    });
    client.stop();
  });
});

it("socket closes a connected client when the scope closes", async () => {
  const busOptions = await options();
  let client: Awaited<ReturnType<typeof connect>> | undefined;
  await withBus(busOptions, async () => {
    client = await connect(busOptions.socketPath);
    await client.take(takeOp("hello"));
  });
  await expect(client?.waitClosed()).resolves.toBeTypeOf("number");
});

it("socket fails to start in a directory that does not exist", async () => {
  const logDir = await tempDir();
  const exit = await runExit(
    Effect.scoped(
      makeEventBus({
        socketPath: join(logDir, "missing", "bus.sock"),
        logPath: join(logDir, "bus.jsonl"),
        maxLag: 16,
      }),
    ),
  );
  expect(Exit.isFailure(exit)).toBe(true);
  const error = Exit.isFailure(exit) ? Option.getOrThrow(Exit.findErrorOption(exit)) : undefined;
  expect(error).toBeInstanceOf(EventBusStartFailed);
  expect(error._tag).toBe("EventBusStartFailed");
  expect(((error as EventBusStartFailed).cause as NodeJS.ErrnoException).code).toBe("EACCES");
});

it("socket fails to start when the socket path is a directory", async () => {
  const dir = await tempDir();
  const socketPath = join(dir, "bus.sock");
  await mkdir(socketPath);
  const exit = await runExit(
    Effect.scoped(makeEventBus({ socketPath, logPath: join(dir, "bus.jsonl"), maxLag: 16 })),
  );
  expect(Exit.isFailure(exit)).toBe(true);
  const error = Exit.isFailure(exit) ? Option.getOrThrow(Exit.findErrorOption(exit)) : undefined;
  expect(error).toBeInstanceOf(EventBusStartFailed);
  expect(error._tag).toBe("EventBusStartFailed");
  expect(((error as EventBusStartFailed).cause as NodeJS.ErrnoException).code).toBe(
    "ERR_FS_EISDIR",
  );
});

it("socket fails to start when the log cannot be opened", async () => {
  const dir = await tempDir();
  const exit = await runExit(
    Effect.scoped(
      makeEventBus({
        socketPath: tempSocket(),
        logPath: join(dir, "missing", "bus.jsonl"),
        maxLag: 16,
      }),
    ),
  );
  const error = Exit.isFailure(exit) ? Option.getOrThrow(Exit.findErrorOption(exit)) : undefined;
  expect(error).toBeInstanceOf(EventBusStartFailed);
  expect(error._tag).toBe("EventBusStartFailed");
  expect(((error as EventBusStartFailed).cause as { _tag?: string })._tag).toBe("LogFailure");
});

it("socket fails to start when the log is corrupt", async () => {
  const dir = await tempDir();
  const logPath = join(dir, "bus.jsonl");
  await writeFile(
    logPath,
    '{"seq":1,"topic":"t","payload":1}\n{"seq":3,"topic":"t","payload":2}\n',
  );
  const exit = await runExit(
    Effect.scoped(makeEventBus({ socketPath: tempSocket(), logPath, maxLag: 16 })),
  );
  const error = Exit.isFailure(exit) ? Option.getOrThrow(Exit.findErrorOption(exit)) : undefined;
  expect(error).toBeInstanceOf(EventBusStartFailed);
  expect(error._tag).toBe("EventBusStartFailed");
  expect(((error as EventBusStartFailed).cause as { _tag?: string })._tag).toBe("LogCorrupt");
});
