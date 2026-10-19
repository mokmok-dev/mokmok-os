import { describe, expect, test } from "vitest";

import {
  MAX_PAYLOAD_BYTES,
  formatBytes,
  parsePayload,
  payloadBytes,
  payloadPreview,
  relativeTime,
} from "./payload.ts";

const EMPTY_MESSAGE = "the payload is empty";
const INVALID_MESSAGE = "the payload is not valid JSON";
const TOO_LARGE_MESSAGE = `the payload is larger than ${MAX_PAYLOAD_BYTES} bytes`;

describe("parsePayload", () => {
  test("accepts every JSON value", () => {
    const values = [
      null,
      true,
      false,
      0,
      -12.5,
      42,
      "text",
      "unicode ✓ é",
      [],
      [1, { nested: null }],
      {},
      { a: true, b: [1, 2], c: "three" },
    ];

    for (const value of values) {
      expect(parsePayload(JSON.stringify(value))).toEqual({
        ok: true,
        payload: value,
      });
    }
  });

  test("ignores surrounding whitespace", () => {
    expect(parsePayload('  {"a":1}  ')).toEqual({ ok: true, payload: { a: 1 } });
    expect(parsePayload("\n\t[1, 2]\r\n")).toEqual({ ok: true, payload: [1, 2] });
  });

  test("rejects an empty document", () => {
    for (const text of ["", " ", "   ", "\t", "\n\r\t "]) {
      expect(parsePayload(text)).toEqual({
        ok: false,
        message: EMPTY_MESSAGE,
      });
    }
  });

  test("rejects text that is not JSON", () => {
    for (const text of [
      "{",
      "tru",
      "'text'",
      '{"a":1,}',
      "[1,]",
      "NaN",
      "Infinity",
      "undefined",
      "1e999",
      '{"a" 1}',
    ]) {
      expect(parsePayload(text)).toEqual({
        ok: false,
        message: INVALID_MESSAGE,
      });
    }
  });

  test("accepts a payload exactly at the byte cap and rejects one byte over", () => {
    const atCap = "a".repeat(MAX_PAYLOAD_BYTES - 2);

    expect(parsePayload(JSON.stringify(atCap))).toEqual({
      ok: true,
      payload: atCap,
    });

    const overCap = "a".repeat(MAX_PAYLOAD_BYTES - 1);

    expect(parsePayload(JSON.stringify(overCap))).toEqual({
      ok: false,
      message: TOO_LARGE_MESSAGE,
    });
  });

  test("counts the cap in UTF-8 bytes rather than in characters", () => {
    const atCap = "é".repeat((MAX_PAYLOAD_BYTES - 2) / 2);

    expect(payloadBytes(atCap)).toBe(MAX_PAYLOAD_BYTES);
    expect(parsePayload(JSON.stringify(atCap)).ok).toBe(true);

    const overCap = `${atCap}é`;

    expect(parsePayload(JSON.stringify(overCap))).toEqual({
      ok: false,
      message: TOO_LARGE_MESSAGE,
    });
  });
});

describe("payloadBytes", () => {
  test("measures the compact UTF-8 JSON encoding", () => {
    const cases: Array<[Parameters<typeof payloadBytes>[0], number]> = [
      [null, 4],
      [true, 4],
      [false, 5],
      [0, 1],
      [-1, 2],
      ["x", 3],
      ["", 2],
      ["é", 4],
      ["✓", 5],
      [[1, 2, 3], 7],
      [{ a: 1 }, 7],
      [{ a: [1, 2] }, 11],
    ];

    for (const [payload, bytes] of cases) {
      expect(payloadBytes(payload)).toBe(bytes);
    }
  });
});

describe("payloadPreview", () => {
  test("renders short payloads as compact JSON", () => {
    expect(payloadPreview({ a: 1 })).toBe('{"a":1}');
    expect(payloadPreview("hi")).toBe('"hi"');
    expect(payloadPreview([1, 2])).toBe("[1,2]");
  });

  test("truncates at the limit and marks the cut with an ellipsis", () => {
    expect(payloadPreview([1, 2, 3], 4)).toBe("[1,2…");
    expect(payloadPreview([1, 2, 3], 3)).toBe("[1,…");
    expect(payloadPreview(0, 0)).toBe("…");
  });

  test("does not truncate a document that fits the limit exactly", () => {
    expect(payloadPreview([1], 3)).toBe("[1]");
    expect(payloadPreview([1], 2)).toBe("[1…");
  });

  test("the default limit is eighty characters", () => {
    const atLimit = "a".repeat(78);
    const overLimit = "a".repeat(79);

    expect(payloadPreview(atLimit)).toBe(JSON.stringify(atLimit));
    expect(payloadPreview(overLimit)).toBe(`${JSON.stringify(overLimit).slice(0, 80)}…`);
  });
});

describe("formatBytes", () => {
  test("formats each unit and its boundary", () => {
    const cases: Array<[number, string]> = [
      [0, "0 B"],
      [1, "1 B"],
      [1023, "1023 B"],
      [1024, "1.0 KB"],
      [1536, "1.5 KB"],
      [1024 * 1024 - 1, "1024.0 KB"],
      [1024 * 1024, "1.0 MB"],
      [1536 * 1024, "1.5 MB"],
      [1024 * 1024 * 1024, "1.0 GB"],
      [1024 * 1024 * 1024 * 1024, "1024.0 GB"],
      [MAX_PAYLOAD_BYTES, "256.0 KB"],
    ];

    for (const [bytes, text] of cases) {
      expect(formatBytes(bytes)).toBe(text);
    }
  });
});

describe("relativeTime", () => {
  test("describes instants in the past and the future", () => {
    const cases: Array<[number, number, string]> = [
      [1000, 1500, "in the future"],
      [1000, 1000, "just now"],
      [999, 0, "just now"],
      [1000, 0, "1s ago"],
      [1999, 0, "1s ago"],
      [2000, 0, "2s ago"],
      [5000, 3500, "1s ago"],
      [59_999, 0, "59s ago"],
      [60_000, 0, "1m ago"],
      [90_000, 0, "1m ago"],
      [3_599_999, 0, "59m ago"],
      [3_600_000, 0, "1h ago"],
      [86_399_999, 0, "23h ago"],
      [86_400_000, 0, "1d ago"],
      [172_800_000, 0, "2d ago"],
    ];

    for (const [nowMs, thenMs, text] of cases) {
      expect(relativeTime(nowMs, thenMs)).toBe(text);
    }
  });
});
