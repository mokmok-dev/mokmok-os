import { expect, it } from "vitest";
import { decodeClientFrame, encodeServerFrame, type ServerFrame } from "../src/wire.ts";

const JSON_SAFE = (value: unknown): unknown => JSON.parse(JSON.stringify(value));

it("client frame decodes a publish frame", () => {
  const text = '{"op":"publish","req":7,"topic":"global","payload":{"kind":"notice"}}';
  expect(decodeClientFrame(text)).toEqual({
    op: "publish",
    req: 7,
    topic: "global",
    payload: { kind: "notice" },
  });
});

it("client frame decodes a subscribe frame", () => {
  expect(decodeClientFrame('{"op":"subscribe","req":0,"topic":"global","after":41}')).toEqual({
    op: "subscribe",
    req: 0,
    topic: "global",
    after: 41,
  });
});

it("client frame rejects text that is not JSON", () => {
  expect(decodeClientFrame("nope")).toBeUndefined();
});

it("client frame rejects an unknown op", () => {
  expect(decodeClientFrame('{"op":"replay","req":1,"topic":"t","after":0}')).toBeUndefined();
});

it("client frame rejects a publish frame without a topic", () => {
  expect(decodeClientFrame('{"op":"publish","req":1,"payload":1}')).toBeUndefined();
});

it("client frame rejects a publish frame without a payload", () => {
  expect(decodeClientFrame('{"op":"publish","req":1,"topic":"t"}')).toBeUndefined();
});

it("client frame rejects a subscribe frame without a request id", () => {
  expect(decodeClientFrame('{"op":"subscribe","topic":"t","after":0}')).toBeUndefined();
});

it("client frame rejects an empty topic", () => {
  expect(decodeClientFrame('{"op":"subscribe","req":1,"topic":"","after":0}')).toBeUndefined();
});

it("client frame rejects a negative after", () => {
  expect(decodeClientFrame('{"op":"subscribe","req":1,"topic":"t","after":-1}')).toBeUndefined();
});

it("client frame rejects a fractional after", () => {
  expect(decodeClientFrame('{"op":"subscribe","req":1,"topic":"t","after":1.5}')).toBeUndefined();
});

it("client frame rejects a negative request id", () => {
  expect(decodeClientFrame('{"op":"publish","req":-2,"topic":"t","payload":1}')).toBeUndefined();
});

const frames: ReadonlyArray<ServerFrame> = [
  { op: "hello", head: 41 },
  { op: "published", req: 7, seq: 42 },
  { op: "event", seq: 42, topic: "global", payload: { kind: "notice" } },
  { op: "error", req: 9, code: "write_failed" },
];

for (const [index, frame] of frames.entries()) {
  it(`server frame encodes frame ${index} exactly`, () => {
    expect(JSON.parse(encodeServerFrame(frame))).toEqual(JSON_SAFE(frame));
  });
}
