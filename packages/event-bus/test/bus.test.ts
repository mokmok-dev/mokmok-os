import fc from "fast-check";
import { Cause, Deferred, Effect, Exit, Fiber, Option, Stream } from "effect";
import { expect, it } from "vitest";
import { type Bus, type From, makeBus } from "../src/bus.ts";
import {
  LogCorrupt,
  LogFailure,
  LogReadFailed,
  PublishFailed,
  SubscriberLagged,
} from "../src/errors.ts";
import { type Event, encodeEventLine, parseLog } from "../src/event.ts";
import { run, waitFor } from "./support/helpers.ts";
import { scriptedLog, type ScriptedLog } from "./support/scripted-log.ts";

const makeTestBus = (
  log: ScriptedLog,
  maxLag = 16,
): Effect.Effect<Bus & { readonly liveSubscriptions: Effect.Effect<number> }, LogCorrupt> =>
  makeBus({ log: log.store, maxLag }).pipe(
    Effect.map(({ bus, liveSubscriptions }) => ({ ...bus, liveSubscriptions })),
  );

fc.configureGlobal({ seed: 20261023, numRuns: 25 });

const collect = (stream: Stream.Stream<Event, unknown>, take: number) =>
  stream.pipe(Stream.take(take), Stream.runCollect);

const failureOf = (exit: Exit.Exit<unknown, unknown>): unknown => {
  if (Exit.isSuccess(exit)) throw new Error("expected a failure");
  return Option.getOrThrow(Cause.findErrorOption(exit.cause));
};

const seeded = (events: ReadonlyArray<Event>): Uint8Array => {
  const chunks = events.map(encodeEventLine);
  const bytes = new Uint8Array(chunks.reduce((size, chunk) => size + chunk.length, 0));
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return bytes;
};

it("publishing starts with a head of zero on an empty log", async () => {
  const head = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(scriptedLog());
      return yield* bus.head;
    }),
  );
  expect(head).toBe(0);
});

it("publishing assigns 1..n and appends each event in sequence order", async () => {
  const log = scriptedLog();
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      const seqs = [];
      for (const payload of [{ n: 1 }, "two", [3]]) {
        seqs.push(yield* bus.publish("topic", payload));
      }
      return { seqs, head: yield* bus.head };
    }),
  );
  expect(result.seqs).toEqual([1, 2, 3]);
  expect(result.head).toBe(3);
  expect(log.lines()).toEqual([
    '{"seq":1,"topic":"topic","payload":{"n":1}}',
    '{"seq":2,"topic":"topic","payload":"two"}',
    '{"seq":3,"topic":"topic","payload":[3]}',
  ]);
  expect(log.events()).toEqual([
    { seq: 1, topic: "topic", payload: { n: 1 } },
    { seq: 2, topic: "topic", payload: "two" },
    { seq: 3, topic: "topic", payload: [3] },
  ]);
});

it("publishing keeps concurrent publishes contiguous and parseable", async () => {
  await fc.assert(
    fc.asyncProperty(
      fc.array(fc.jsonValue({ maxDepth: 2 }), { minLength: 1, maxLength: 16 }),
      async (payloads) => {
        const log = scriptedLog();
        const seqs = await run(
          Effect.gen(function* () {
            const bus = yield* makeTestBus(log, 64);
            return yield* Effect.forEach(payloads, (payload) => bus.publish("t", payload), {
              concurrency: "unbounded",
            });
          }),
        );
        expect([...seqs].sort((left, right) => left - right)).toEqual(
          payloads.map((_, index) => index + 1),
        );
        const events = log.events();
        expect(events).toHaveLength(payloads.length);
        expect(events.map((event) => event.seq)).toEqual(payloads.map((_, index) => index + 1));
        expect(new Set(events.map((event) => JSON.stringify(event.payload)))).toEqual(
          new Set(payloads.map((payload) => JSON.stringify(payload))),
        );
      },
    ),
    { numRuns: 25 },
  );
});

it("publishing reports an append failure as PublishFailed and leaves the head unchanged", async () => {
  const log = scriptedLog({ failAppends: 1 });
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      const failure = yield* Effect.exit(bus.publish("t", "first"));
      const head = yield* bus.head;
      const retried = yield* bus.publish("t", "second");
      return { failure, head, retried, finalHead: yield* bus.head };
    }),
  );
  const error = failureOf(result.failure);
  expect(error).toBeInstanceOf(PublishFailed);
  expect(error._tag).toBe("PublishFailed");
  expect((error as PublishFailed).cause).toBeInstanceOf(LogFailure);
  expect(result.head).toBe(0);
  expect(result.retried).toBe(1);
  expect(result.finalHead).toBe(1);
  expect(log.events()).toEqual([{ seq: 1, topic: "t", payload: "second" }]);
});

it("publishing fails a non-JSON payload before appending anything", async () => {
  const log = scriptedLog();
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      const cycle: Record<string, unknown> = {};
      cycle["self"] = cycle;
      const payloads: ReadonlyArray<unknown> = [undefined, () => 1, cycle, Symbol("s")];
      const outcomes = [];
      for (const payload of payloads) {
        outcomes.push(yield* Effect.exit(bus.publish("t", payload as never)));
      }
      return { outcomes, head: yield* bus.head };
    }),
  );
  for (const outcome of result.outcomes) {
    expect(failureOf(outcome)).toBeInstanceOf(PublishFailed);
  }
  expect(result.head).toBe(0);
  expect(log.appends()).toBe(0);
});

it("publishing carries the rejected payload as the cause of PublishFailed", async () => {
  const rejected = () => 1;
  const failure = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(scriptedLog());
      return yield* Effect.exit(bus.publish("t", rejected as never));
    }),
  );
  const error = failureOf(failure) as PublishFailed;
  expect(error._tag).toBe("PublishFailed");
  expect(error.cause).toBe(rejected);
});

const seededLog = (): ScriptedLog =>
  scriptedLog({
    initial: seeded([
      { seq: 1, topic: "t", payload: "one" },
      { seq: 2, topic: "other", payload: "x" },
      { seq: 3, topic: "t", payload: "three" },
    ]),
  });

for (const [label, from, expected] of [
  ["beginning", "beginning", [1, 3]],
  ["after 0", { after: 0 }, [1, 3]],
  ["after 1", { after: 1 }, [3]],
] as ReadonlyArray<[string, From, ReadonlyArray<number>]>) {
  it(`subscribing replays the durable prefix for ${label}`, async () => {
    const log = seededLog();
    const events = await run(
      Effect.gen(function* () {
        const bus = yield* makeTestBus(log);
        return yield* collect(bus.subscribe("t", from), expected.length);
      }),
    );
    expect(events.map((event) => event.seq)).toEqual(expected);
  });
}

for (const [label, from] of [
  ["live", "live"],
  ["an offset at the head", { after: 3 }],
] as ReadonlyArray<[string, From]>) {
  it(`subscribing replays nothing for ${label}`, async () => {
    const log = seededLog();
    const events = await run(
      Effect.gen(function* () {
        const bus = yield* makeTestBus(log);
        const fiber = yield* Effect.forkChild(collect(bus.subscribe("t", from), 1));
        yield* waitFor(() => log.reads() === 2);
        yield* bus.publish("t", "four");
        return yield* Fiber.join(fiber);
      }),
    );
    expect(events.map((event) => event.seq)).toEqual([4]);
  });
}

it("subscribing replays a live subscription from the head it started at", async () => {
  const log = seededLog();
  const events = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      const fiber = yield* Effect.forkChild(collect(bus.subscribe("t", "live"), 1));
      yield* waitFor(() => log.reads() === 2);
      yield* bus.publish("t", "four");
      return yield* Fiber.join(fiber);
    }),
  );
  expect(events).toEqual([{ seq: 4, topic: "t", payload: "four" }]);
});

it("subscribing delivers only the events of the subscribed topic", async () => {
  const log = scriptedLog();
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      const other = yield* Effect.forkChild(collect(bus.subscribe("other", "live"), 1));
      const mine = yield* Effect.forkChild(collect(bus.subscribe("t", "live"), 1));
      yield* waitFor(() => log.reads() === 3);
      yield* bus.publish("t", "first");
      yield* bus.publish("other", "second");
      return { mine: yield* Fiber.join(mine), other: yield* Fiber.join(other) };
    }),
  );
  expect(result.mine).toEqual([{ seq: 1, topic: "t", payload: "first" }]);
  expect(result.other).toEqual([{ seq: 2, topic: "other", payload: "second" }]);
});

it("subscribing delivers every event to both subscribers of one topic", async () => {
  const log = scriptedLog();
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      const first = yield* Effect.forkChild(collect(bus.subscribe("t", "live"), 2));
      const second = yield* Effect.forkChild(collect(bus.subscribe("t", "live"), 2));
      yield* waitFor(() => log.reads() === 3);
      yield* bus.publish("t", 1);
      yield* bus.publish("t", 2);
      return { first: yield* Fiber.join(first), second: yield* Fiber.join(second) };
    }),
  );
  expect(result.first.map((event) => event.seq)).toEqual([1, 2]);
  expect(result.second.map((event) => event.seq)).toEqual([1, 2]);
});

it("subscribing releases an event to subscribers only after its append is synced", async () => {
  const log = scriptedLog();
  const gate = await run(Deferred.make<void>());
  const entered = await run(Deferred.make<void>());
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      const seen: Array<number> = [];
      const fiber = yield* Effect.forkChild(
        bus.subscribe("t", "live").pipe(
          Stream.runForEach((event) =>
            Effect.sync(() => {
              seen.push(event.seq);
            }),
          ),
        ),
      );
      yield* waitFor(() => log.reads() === 2);
      log.armAppendGate(gate, entered);
      const publishing = yield* Effect.forkChild(bus.publish("t", "value"));
      yield* Deferred.await(entered);
      const beforeSync = [...seen];
      yield* Deferred.succeed(gate, void 0);
      yield* Fiber.join(publishing);
      yield* waitFor(() => seen.length === 1);
      yield* Fiber.interrupt(fiber);
      return beforeSync;
    }),
  );
  expect(result).toEqual([]);
});

it("subscribing delivers a strictly increasing, hole-free sequence to a subscriber that joins while publishes run", async () => {
  await fc.assert(
    fc.asyncProperty(
      fc.array(fc.jsonValue({ maxDepth: 2 }), { minLength: 1, maxLength: 20 }),
      async (payloads) => {
        const log = scriptedLog();
        const seen = await run(
          Effect.gen(function* () {
            const bus = yield* makeTestBus(log, 64);
            const publishing = yield* Effect.forkChild(
              Effect.forEach(payloads, (payload) => bus.publish("t", payload), {
                concurrency: "unbounded",
              }),
            );
            const collected = yield* bus.subscribe("t", "beginning").pipe(
              Stream.takeUntil((event) => event.seq === payloads.length),
              Stream.runCollect,
            );
            yield* Fiber.join(publishing);
            return collected;
          }),
        );
        expect(seen.map((event) => event.seq)).toEqual(payloads.map((_, index) => index + 1));
        expect(seen.map((event) => JSON.parse(JSON.stringify(event.payload)))).toEqual(
          JSON.parse(JSON.stringify(payloads)),
        );
      },
    ),
    { numRuns: 25 },
  );
});

it("subscribing replays before going live and delivers the boundary event exactly once", async () => {
  const log = scriptedLog();
  const gate = await run(Deferred.make<void>());
  const entered = await run(Deferred.make<void>());
  const done = await run(Deferred.make<void>());
  const events = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      yield* bus.publish("t", "a");
      const fiber = yield* Effect.forkChild(collect(bus.subscribe("t", "beginning"), 3));
      log.armReadGate(gate, entered, done);
      yield* Deferred.await(entered);
      yield* bus.publish("t", "b");
      yield* Deferred.succeed(gate, void 0);
      yield* Deferred.await(done);
      yield* bus.publish("t", "c");
      return yield* Fiber.join(fiber);
    }),
  );
  expect(events).toEqual([
    { seq: 1, topic: "t", payload: "a" },
    { seq: 2, topic: "t", payload: "b" },
    { seq: 3, topic: "t", payload: "c" },
  ]);
});

it("subscribing reports a failed replay read as LogReadFailed and unregisters", async () => {
  const log = scriptedLog();
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      log.failNextRead();
      const exit = yield* Effect.exit(collect(bus.subscribe("t", "beginning"), 1));
      return { exit, live: yield* bus.liveSubscriptions };
    }),
  );
  const error = failureOf(result.exit);
  expect(error).toBeInstanceOf(LogReadFailed);
  expect(error._tag).toBe("LogReadFailed");
  expect((error as LogReadFailed).cause).toBeInstanceOf(LogFailure);
  expect(result.live).toBe(0);
});

it("subscribing reports a corrupt replay read as LogReadFailed and unregisters", async () => {
  const log = scriptedLog();
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      log.setBytes(new TextEncoder().encode("garbage\n"));
      const exit = yield* Effect.exit(collect(bus.subscribe("t", "beginning"), 1));
      return { exit, live: yield* bus.liveSubscriptions };
    }),
  );
  const error = failureOf(result.exit);
  expect(error._tag).toBe("LogReadFailed");
  expect((error as LogReadFailed).cause).toBeInstanceOf(LogCorrupt);
  expect(result.live).toBe(0);
});

it("subscribing fails a subscription that falls more than maxLag behind", async () => {
  const log = scriptedLog();
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log, 1);
      const block = yield* Deferred.make<void>();
      const seen: Array<number> = [];
      const fiber = yield* Effect.forkChild(
        bus.subscribe("t", "live").pipe(
          Stream.runForEach((event) =>
            Effect.andThen(
              Effect.sync(() => {
                seen.push(event.seq);
              }),
              Deferred.await(block),
            ),
          ),
          Effect.exit,
        ),
      );
      yield* waitFor(() => log.reads() === 2);
      yield* bus.publish("t", 1);
      yield* waitFor(() => seen.length === 1);
      yield* bus.publish("t", 2);
      yield* bus.publish("t", 3);
      yield* Deferred.succeed(block, void 0);
      const exit = yield* Fiber.join(fiber);
      const after = yield* Effect.exit(bus.publish("t", 4));
      return { seen, exit, after, head: yield* bus.head };
    }),
  );
  expect(result.seen).toEqual([1, 2]);
  expect(failureOf(result.exit)).toBeInstanceOf(SubscriberLagged);
  expect(Exit.isSuccess(result.after)).toBe(true);
  expect(result.head).toBe(4);
});

it("subscribing unregisters a subscription when its stream ends and leaves the others intact", async () => {
  const log = scriptedLog();
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      const leaving = yield* Effect.forkChild(collect(bus.subscribe("t", "live"), 1));
      const staying = yield* Effect.forkChild(collect(bus.subscribe("t", "live"), 2));
      yield* waitFor(() => log.reads() === 3);
      const both = yield* bus.liveSubscriptions;
      yield* bus.publish("t", 1);
      const left = yield* Fiber.join(leaving);
      const afterFirst = yield* bus.liveSubscriptions;
      yield* bus.publish("t", 2);
      const stayed = yield* Fiber.join(staying);
      return {
        both,
        afterFirst,
        afterBoth: yield* bus.liveSubscriptions,
        left,
        stayed,
      };
    }),
  );
  expect(result.both).toBe(2);
  expect(result.afterFirst).toBe(1);
  expect(result.afterBoth).toBe(0);
  expect(result.left.map((event) => event.seq)).toEqual([1]);
  expect(result.stayed.map((event) => event.seq)).toEqual([1, 2]);
});

it("commit lock leaves the log and the head consistent when a publish is interrupted mid-commit", async () => {
  const log = scriptedLog();
  const gate = await run(Deferred.make<void>());
  const entered = await run(Deferred.make<void>());
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      log.armAppendGate(gate, entered);
      const publishing = yield* Effect.forkChild(bus.publish("t", "value"));
      yield* Deferred.await(entered);
      const started = yield* Deferred.make<void>();
      const interrupting = yield* Effect.forkChild(
        Effect.andThen(Deferred.succeed(started, void 0), Fiber.interrupt(publishing)),
      );
      yield* Deferred.await(started);
      yield* Effect.yieldNow;
      yield* Deferred.succeed(gate, void 0);
      yield* Fiber.join(interrupting);
      const head = yield* bus.head;
      const next = yield* bus.publish("t", "next");
      return { head, next, events: log.events(), finalHead: yield* bus.head };
    }),
  );
  expect(result.head).toBe(1);
  expect(result.events).toEqual([
    { seq: 1, topic: "t", payload: "value" },
    { seq: 2, topic: "t", payload: "next" },
  ]);
  expect(result.next).toBe(2);
  expect(result.finalHead).toBe(2);
});

it("recovery recovers the head from a complete log and truncates it to its valid length", async () => {
  const log = scriptedLog({
    initial: seeded([
      { seq: 1, topic: "a", payload: 1 },
      { seq: 2, topic: "b", payload: 2 },
    ]),
  });
  const length = log.bytes().length;
  const head = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      return yield* bus.head;
    }),
  );
  expect(head).toBe(2);
  expect(log.truncations()).toEqual([length]);
});

it("recovery truncates a torn tail and continues from the recovered head", async () => {
  const complete = seeded([{ seq: 1, topic: "a", payload: 1 }]);
  const initial = new Uint8Array([...complete, ...new TextEncoder().encode('{"seq":2,"topic')]);
  const log = scriptedLog({ initial });
  const result = await run(
    Effect.gen(function* () {
      const bus = yield* makeTestBus(log);
      const head = yield* bus.head;
      const seq = yield* bus.publish("a", 2);
      return { head, seq, bytes: log.bytes().length };
    }),
  );
  expect(result.head).toBe(1);
  expect(result.seq).toBe(2);
  expect(log.events()).toEqual([
    { seq: 1, topic: "a", payload: 1 },
    { seq: 2, topic: "a", payload: 2 },
  ]);
  expect(log.truncations()).toEqual([complete.length]);
});

it("recovery fails to start on a corrupt complete line", async () => {
  const log = scriptedLog({
    initial: new TextEncoder().encode(
      '{"seq":1,"topic":"a","payload":1}\n{"seq":3,"topic":"a","payload":2}\n',
    ),
  });
  const exit = await run(Effect.exit(makeTestBus(log)));
  const error = failureOf(exit);
  expect(error).toBeInstanceOf(LogCorrupt);
  expect(error._tag).toBe("LogCorrupt");
  expect((error as LogCorrupt).cause).toBeInstanceOf(LogCorrupt);
});

it("recovery does not append when recovery fails", async () => {
  const log = scriptedLog({ initial: new TextEncoder().encode("garbage\n") });
  await run(Effect.exit(makeTestBus(log)));
  expect(log.appends()).toBe(0);
  expect(log.truncations()).toEqual([]);
});

it("from values starts a live subscription with nothing to replay", () => {
  expect(parseLog(new Uint8Array(0)).events).toEqual([]);
});
