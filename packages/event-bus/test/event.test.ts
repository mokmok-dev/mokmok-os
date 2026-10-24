import fc from "fast-check";
import { expect, it } from "vitest";
import { LogCorrupt } from "../src/errors.ts";
import { encodeEventLine, type Event, parseLog } from "../src/event.ts";

const utf8 = (text: string): Uint8Array => new TextEncoder().encode(text);

fc.configureGlobal({ seed: 20261023, numRuns: 100 });

const encodeLog = (events: ReadonlyArray<Event>): Uint8Array => {
  const chunks = events.map(encodeEventLine);
  const total = chunks.reduce((size, chunk) => size + chunk.length, 0);
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return bytes;
};

const bodyArb = fc.record({
  topic: fc.string({ unit: "grapheme" }),
  payload: fc.jsonValue({ maxDepth: 3 }),
});

const logArb = fc
  .array(bodyArb, { maxLength: 12 })
  .map((bodies) => bodies.map((body, index): Event => ({ seq: index + 1, ...body })));

const corrupt = (run: () => unknown): LogCorrupt => {
  try {
    run();
  } catch (error) {
    if (error instanceof LogCorrupt) return error;
    throw error;
  }
  throw new Error("expected LogCorrupt");
};

it("event log round-trips arbitrary events and consumes every byte", () => {
  fc.assert(
    fc.property(logArb, (events) => {
      const bytes = encodeLog(events);
      const parsed = parseLog(bytes);
      expect(parsed.events).toEqual(JSON.parse(JSON.stringify(events)));
      expect(parsed.validBytes).toBe(bytes.length);
    }),
  );
});

it("event log ends every encoded line with a newline", () => {
  const line = encodeEventLine({ seq: 1, topic: "t", payload: 1 });
  expect(new TextDecoder().decode(line).endsWith("\n")).toBe(true);
});

it("event log round-trips a payload that contains a newline", () => {
  const event: Event = { seq: 1, topic: "t", payload: { text: "one\ntwo" } };
  const parsed = parseLog(encodeEventLine(event));
  expect(parsed.events).toEqual([event]);
});

it("event log parses two newline-terminated lines into two events", () => {
  const bytes = encodeLog([
    { seq: 1, topic: "a", payload: null },
    { seq: 2, topic: "b", payload: [1, 2] },
  ]);
  const parsed = parseLog(bytes);
  expect(parsed.events.map((event) => event.seq)).toEqual([1, 2]);
  expect(parsed.validBytes).toBe(bytes.length);
});

it("event log parses an empty log into nothing", () => {
  expect(parseLog(new Uint8Array(0))).toEqual({ events: [], validBytes: 0 });
});

it("event log ignores a torn trailing line and reports the last accepted offset", () => {
  const complete = encodeLog([{ seq: 1, topic: "a", payload: 1 }]);
  const torn = new Uint8Array([...complete, ...utf8('{"seq":2,"topic":"b"')]);
  const parsed = parseLog(torn);
  expect(parsed.events.map((event) => event.seq)).toEqual([1]);
  expect(parsed.validBytes).toBe(complete.length);
});

it("event log treats a log of only a torn line as empty", () => {
  const parsed = parseLog(utf8('{"seq":1'));
  expect(parsed.events).toEqual([]);
  expect(parsed.validBytes).toBe(0);
});

it("event log throws LogCorrupt naming a complete line that is not an event", () => {
  const error = corrupt(() => parseLog(utf8('{"nope":true}\n')));
  expect(error.cause).toBe('{"nope":true}');
});

it("event log throws LogCorrupt on a complete line that is not JSON", () => {
  expect(corrupt(() => parseLog(utf8("not json\n"))).cause).toBe("not json");
});

it("event log throws LogCorrupt on a sequence gap", () => {
  const bytes = encodeLog([
    { seq: 1, topic: "a", payload: 1 },
    { seq: 3, topic: "a", payload: 2 },
  ]);
  expect(corrupt(() => parseLog(bytes)).cause).toEqual({ seq: 3, topic: "a", payload: 2 });
});

it("event log throws LogCorrupt on a duplicate sequence", () => {
  const bytes = encodeLog([
    { seq: 1, topic: "a", payload: 1 },
    { seq: 1, topic: "a", payload: 2 },
  ]);
  expect(() => parseLog(bytes)).toThrow(LogCorrupt);
});

it("event log throws LogCorrupt on a log that starts at sequence two", () => {
  expect(() => parseLog(encodeEventLine({ seq: 2, topic: "a", payload: 1 }))).toThrow(LogCorrupt);
});

it("event log throws LogCorrupt on a line whose sequence is not a natural number", () => {
  expect(() => parseLog(utf8('{"seq":1.5,"topic":"a","payload":1}\n'))).toThrow(LogCorrupt);
});

it("event log keeps the valid prefix parseable and complete after a torn tail", () => {
  fc.assert(
    fc.property(
      logArb,
      fc.uint8Array().filter((tail) => !tail.includes(0x0a)),
      (events, tail) => {
        const bytes = encodeLog(events);
        const combined = new Uint8Array([...bytes, ...tail]);
        const parsed = parseLog(combined);
        const prefix = combined.subarray(0, parsed.validBytes);
        const reparsed = parseLog(prefix);
        expect(reparsed.events).toEqual(parsed.events);
        expect(reparsed.validBytes).toBe(prefix.length);
      },
    ),
  );
});

it("event log never yields non-contiguous events from arbitrary bytes", () => {
  fc.assert(
    fc.property(fc.uint8Array(), (bytes) => {
      let parsed: ReturnType<typeof parseLog>;
      try {
        parsed = parseLog(bytes);
      } catch (error) {
        expect(error).toBeInstanceOf(LogCorrupt);
        return;
      }
      expect(parsed.validBytes).toBeLessThanOrEqual(bytes.length);
      parsed.events.forEach((event, index) => {
        expect(event.seq).toBe(index + 1);
      });
      const prefix = bytes.subarray(0, parsed.validBytes);
      const reparsed = parseLog(prefix);
      expect(reparsed.events).toEqual(parsed.events);
      expect(reparsed.validBytes).toBe(prefix.length);
    }),
  );
});
