import { describe, expect, test } from "vitest";

import * as Wire from "./wire.ts";

const EPOCH = "2904014b-406d-4859-b9d7-657242a21c51";
const OTHER_EPOCH = "2904014b-406d-4859-b9d7-657242a21c52";
const EVENT_ID = "01a153b9-c165-769d-9b5c-1b69d33f2db6";
const SUBSCRIBER_ID = "018f2f3e-0000-7000-8000-000000000001";
const AT = { secs_since_epoch: 1_792_406_045, nanos_since_epoch: 29_207_030 };
const WINDOW = { epoch: EPOCH, first: 0, next: 1 };
const CURSOR = { epoch: EPOCH, seq: 1 };
const EVENT = { id: EVENT_ID, topic: "global", seq: 0, at: AT, payload: { hello: "web" } };
const LAPSED = { held: CURSOR, from: 9 };

describe("the request frames the bus accepts", () => {
  test("an open frame names a topic and a nullable subscriber", () => {
    expect(
      Wire.decodeBusRequest({ op: "open", topic: "global", subscriber: null }),
    ).toEqual({ op: "open", topic: "global", subscriber: null });

    const resumed = Wire.decodeBusRequest({
      op: "open",
      topic: `session:${EPOCH}`,
      subscriber: SUBSCRIBER_ID,
    });

    expect(resumed).toEqual({
      op: "open",
      topic: `session:${EPOCH}`,
      subscriber: SUBSCRIBER_ID,
    });
  });

  test("a publish frame carries any JSON value as its payload", () => {
    const payloads = [
      { turn: 7 },
      [1, 2, 3],
      "text",
      42,
      0,
      true,
      false,
      null,
    ];

    for (const payload of payloads) {
      expect(Wire.decodeBusRequest({ op: "publish", payload })).toEqual({
        op: "publish",
        payload,
      });
    }
  });

  test("a pull frame asks for a positive whole number of events", () => {
    expect(Wire.decodeBusRequest({ op: "pull", max: 1 })).toEqual({ op: "pull", max: 1 });
    expect(Wire.decodeBusRequest({ op: "pull", max: 128 })).toEqual({ op: "pull", max: 128 });
  });

  test("an ack frame names the position it advances to", () => {
    expect(Wire.decodeBusRequest({ op: "ack", through: CURSOR })).toEqual({
      op: "ack",
      through: CURSOR,
    });
  });

  test("anything that is not exactly one request frame is rejected", () => {
    const invalid: unknown[] = [
      null,
      undefined,
      "open",
      [],
      {},
      { op: "opens", topic: "global", subscriber: null },
      { op: "open", topic: "global" },
      { op: "open", topic: "everything", subscriber: null },
      { op: "open", topic: "agent:not-a-uuid", subscriber: null },
      { op: "open", topic: "global", subscriber: "not-a-uuid" },
      { op: "publish" },
      { op: "publish", payload: undefined },
      { op: "pull" },
      { op: "pull", max: 0 },
      { op: "pull", max: -1 },
      { op: "pull", max: 1.5 },
      { op: "pull", max: "10" },
      { op: "ack" },
      { op: "ack", through: { epoch: EPOCH, seq: -1 } },
      { op: "ack", through: { epoch: "nope", seq: 1 } },
      { op: "ack", through: { epoch: EPOCH } },
    ];

    for (const frame of invalid) {
      expect(Wire.decodeBusRequest(frame)).toBeNull();
    }
  });
});

describe("the response frames the bus sends", () => {
  test("opened names the retained window", () => {
    expect(Wire.decodeBusResponse({ op: "opened", window: WINDOW })).toEqual({
      op: "opened",
      window: WINDOW,
    });
  });

  test("published carries a receipt", () => {
    expect(
      Wire.decodeBusResponse({ op: "published", id: EVENT_ID, seq: 7, at: AT }),
    ).toEqual({ op: "published", id: EVENT_ID, seq: 7, at: AT });
  });

  test("a batch carries its window, an optional lapse, and its events", () => {
    expect(
      Wire.decodeBusResponse({ op: "batch", window: WINDOW, lapsed: null, events: [EVENT] }),
    ).toEqual({ op: "batch", window: WINDOW, lapsed: null, events: [EVENT] });

    expect(
      Wire.decodeBusResponse({ op: "batch", window: WINDOW, lapsed: LAPSED, events: [] }),
    ).toEqual({ op: "batch", window: WINDOW, lapsed: LAPSED, events: [] });
  });

  test("acked echoes the position it advanced to", () => {
    expect(Wire.decodeBusResponse({ op: "acked", through: CURSOR })).toEqual({
      op: "acked",
      through: CURSOR,
    });
  });

  test("a refusal is either a unit reason or an unresumable window", () => {
    for (const reason of ["topic_limit", "connection_limit", "shutting_down"]) {
      expect(Wire.decodeBusResponse({ op: "refused", reason })).toEqual({
        op: "refused",
        reason,
      });
    }

    expect(
      Wire.decodeBusResponse({ op: "refused", reason: { unresumable: { window: WINDOW } } }),
    ).toEqual({ op: "refused", reason: { unresumable: { window: WINDOW } } });
  });

  test("a frame that is not exactly one response is rejected", () => {
    const invalid: unknown[] = [
      null,
      {},
      { op: "opened" },
      { op: "opened", window: { epoch: "nope", first: 0, next: 1 } },
      { op: "published", id: EVENT_ID, seq: 1 },
      { op: "published", id: "nope", seq: 1, at: AT },
      { op: "batch", window: WINDOW, events: [] },
      { op: "batch", window: WINDOW, lapsed: null, events: [{}] },
      { op: "batch", window: WINDOW, lapsed: { held: CURSOR }, events: [] },
      { op: "batch", window: WINDOW, lapsed: { from: 9 }, events: [] },
      { op: "batch", window: WINDOW, lapsed: { held: { epoch: "nope", seq: 1 }, from: 9 }, events: [] },
      { op: "batch", window: WINDOW, lapsed: { held: CURSOR, from: -1 }, events: [] },
      { op: "batch", window: WINDOW, lapsed: { held: CURSOR, from: 1.5 }, events: [] },
      { op: "acked" },
      { op: "refused", reason: "unknown" },
      { op: "refused", reason: { unresumable: { window: { epoch: EPOCH } } } },
      "opened",
    ];

    for (const frame of invalid) {
      expect(Wire.decodeBusResponse(frame)).toBeNull();
    }
  });

  test("a response parsed from text is decoded and a malformed one is not", () => {
    expect(Wire.parseBusResponse(JSON.stringify({ op: "opened", window: WINDOW }))).toEqual({
      op: "opened",
      window: WINDOW,
    });
    expect(Wire.parseBusResponse("{")).toBeNull();
    expect(Wire.parseBusResponse("null")).toBeNull();
    expect(Wire.parseBusResponse(JSON.stringify({ op: "opened" }))).toBeNull();
  });
});

describe("the topics the bus routes", () => {
  test("global and the three identifier kinds are canonical", () => {
    for (const topic of [
      "global",
      `agent:${EPOCH}`,
      `session:${EPOCH}`,
      `execution:${EPOCH}`,
    ]) {
      expect(Wire.decodeBusRequest({ op: "open", topic, subscriber: null })).not.toBeNull();
    }
  });

  test("topic names outside the grammar are rejected", () => {
    for (const topic of [
      "GLOBAL",
      "Global",
      "global ",
      " all",
      "agent",
      "agent:",
      `agents:${EPOCH}`,
      `agent:${EPOCH}x`,
      "session:not-a-uuid",
      `execution:${EPOCH}-0000`,
      ` :${EPOCH}`,
    ]) {
      expect(Wire.decodeBusRequest({ op: "open", topic, subscriber: null })).toBeNull();
    }
  });
});

describe("the sequence and instant bounds", () => {
  test("a sequence is a non-negative safe whole number", () => {
    for (const seq of [0, 1, 42, Number.MAX_SAFE_INTEGER]) {
      expect(Wire.decodeBusRequest({ op: "ack", through: { epoch: EPOCH, seq } })).not.toBeNull();
    }

    for (const seq of [-1, 1.5, Number.MAX_SAFE_INTEGER + 1, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(Wire.decodeBusRequest({ op: "ack", through: { epoch: EPOCH, seq } })).toBeNull();
    }
  });

  test("the nanosecond field is within one second", () => {
    for (const nanos of [0, 1, 999_999_999]) {
      expect(
        Wire.decodeBusResponse({
          op: "published",
          id: EVENT_ID,
          seq: 0,
          at: { secs_since_epoch: 0, nanos_since_epoch: nanos },
        }),
      ).not.toBeNull();
    }

    for (const nanos of [-1, 1_000_000_000, 1.5]) {
      expect(
        Wire.decodeBusResponse({
          op: "published",
          id: EVENT_ID,
          seq: 0,
          at: { secs_since_epoch: 0, nanos_since_epoch: nanos },
        }),
      ).toBeNull();
    }
  });

  test("an instant converts to milliseconds and back", () => {
    expect(Wire.instantMillis({ secs_since_epoch: 0, nanos_since_epoch: 0 })).toBe(0);
    expect(Wire.instantMillis({ secs_since_epoch: 0, nanos_since_epoch: 999_999_999 })).toBe(999);
    expect(Wire.instantMillis({ secs_since_epoch: 1, nanos_since_epoch: 0 })).toBe(1000);
    expect(Wire.instantMillis({ secs_since_epoch: 2, nanos_since_epoch: 1_000_000 })).toBe(2001);

    expect(Wire.atFromMillis(0)).toEqual({ secs_since_epoch: 0, nanos_since_epoch: 0 });
    expect(Wire.atFromMillis(1_999)).toEqual({
      secs_since_epoch: 1,
      nanos_since_epoch: 999_000_000,
    });
    expect(Wire.atFromMillis(2_001)).toEqual({
      secs_since_epoch: 2,
      nanos_since_epoch: 1_000_000,
    });
    expect(Wire.instantMillis(Wire.atFromMillis(1_792_406_045_029))).toBe(1_792_406_045_029);
  });
});

describe("the refusal vocabulary", () => {
  test("each refusal explains itself", () => {
    expect(Wire.describeRefusal("topic_limit")).toBe("the bus has no room for another topic");
    expect(Wire.describeRefusal("connection_limit")).toBe(
      "the bus is at its connection ceiling",
    );
    expect(Wire.describeRefusal("shutting_down")).toBe("the bus is shutting down");
    expect(
      Wire.describeRefusal({ unresumable: { window: { epoch: EPOCH, first: 3, next: 9 } } }),
    ).toBe("the log holds 3..9 and cannot be read from that cursor");
  });
});

describe("the encoded requests", () => {
  test("each request encodes to the wire grammar", () => {
    expect(Wire.encodeBusRequest({ op: "open", topic: "global", subscriber: null })).toBe(
      '{"op":"open","topic":"global","subscriber":null}',
    );
    expect(
      Wire.encodeBusRequest({
        op: "open",
        topic: `session:${EPOCH}`,
        subscriber: SUBSCRIBER_ID,
      }),
    ).toBe(`{"op":"open","topic":"session:${EPOCH}","subscriber":"${SUBSCRIBER_ID}"}`);
    expect(Wire.encodeBusRequest({ op: "publish", payload: { turn: 7 } })).toBe(
      '{"op":"publish","payload":{"turn":7}}',
    );
    expect(Wire.encodeBusRequest({ op: "pull", max: 128 })).toBe('{"op":"pull","max":128}');
    expect(Wire.encodeBusRequest({ op: "ack", through: CURSOR })).toBe(
      `{"op":"ack","through":{"epoch":"${EPOCH}","seq":1}}`,
    );
  });

  test("an encoded request decodes back to itself", () => {
    const request: Wire.BusRequest = { op: "ack", through: { epoch: EPOCH, seq: 5 } };

    expect(Wire.decodeBusRequest(JSON.parse(Wire.encodeBusRequest(request)))).toEqual(request);
  });

  test("the other epoch is a different cursor", () => {
    expect(Wire.decodeBusRequest({ op: "ack", through: { epoch: OTHER_EPOCH, seq: 1 } })).toEqual({
      op: "ack",
      through: { epoch: OTHER_EPOCH, seq: 1 },
    });
  });
});
