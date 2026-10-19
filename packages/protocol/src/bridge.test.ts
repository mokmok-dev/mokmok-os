import { describe, expect, test } from "vitest";

import * as Bridge from "./bridge.ts";

const EPOCH = "2904014b-406d-4859-b9d7-657242a21c51";
const EVENT_ID = "01a153b9-c165-769d-9b5c-1b69d33f2db6";
const SUBSCRIBER_ID = "018f2f3e-0000-7000-8000-000000000001";
const AT = { secs_since_epoch: 1_792_406_045, nanos_since_epoch: 29_207_030 };
const WINDOW = { epoch: EPOCH, first: 0, next: 1 };
const CURSOR = { epoch: EPOCH, seq: 1 };
const EVENT = { id: EVENT_ID, topic: "global", seq: 0, at: AT, payload: { hello: "web" } };
const LAPSED = { held: CURSOR, from: 9 };

describe("the frames the bridge sends to a browser", () => {
  test("hello names the topic and the subscriber", () => {
    expect(
      Bridge.decodeBridgeServerFrame({
        op: "hello",
        topic: "global",
        subscriber: SUBSCRIBER_ID,
      }),
    ).toEqual({ op: "hello", topic: "global", subscriber: SUBSCRIBER_ID });
  });

  test("status reports each connection phase and an optional detail", () => {
    for (const connection of ["connecting", "live", "refused", "offline"]) {
      expect(
        Bridge.decodeBridgeServerFrame({ op: "status", connection, detail: null }),
      ).toEqual({ op: "status", connection, detail: null });
    }

    expect(
      Bridge.decodeBridgeServerFrame({ op: "status", connection: "refused", detail: "no room" }),
    ).toEqual({ op: "status", connection: "refused", detail: "no room" });
  });

  test("a batch forwards the bus frame with an identity", () => {
    expect(
      Bridge.decodeBridgeServerFrame({
        op: "batch",
        batch_id: "batch-1",
        window: WINDOW,
        lapsed: null,
        events: [EVENT],
      }),
    ).toEqual({ op: "batch", batch_id: "batch-1", window: WINDOW, lapsed: null, events: [EVENT] });

    expect(
      Bridge.decodeBridgeServerFrame({
        op: "batch",
        batch_id: "batch-2",
        window: WINDOW,
        lapsed: LAPSED,
        events: [],
      }),
    ).toEqual({ op: "batch", batch_id: "batch-2", window: WINDOW, lapsed: LAPSED, events: [] });
  });

  test("a published frame carries the receipt", () => {
    expect(
      Bridge.decodeBridgeServerFrame({
        op: "published",
        request_id: "request-1",
        id: EVENT_ID,
        seq: 3,
        at: AT,
      }),
    ).toEqual({ op: "published", request_id: "request-1", id: EVENT_ID, seq: 3, at: AT });
  });

  test("a failed publish names the request and whether the append may have happened", () => {
    for (const indeterminate of [true, false]) {
      expect(
        Bridge.decodeBridgeServerFrame({
          op: "publish_failed",
          request_id: "request-1",
          detail: "the bus refused the payload",
          indeterminate,
        }),
      ).toEqual({
        op: "publish_failed",
        request_id: "request-1",
        detail: "the bus refused the payload",
        indeterminate,
      });
    }
  });

  test("an acked frame carries the advanced cursor", () => {
    expect(
      Bridge.decodeBridgeServerFrame({ op: "acked", through: CURSOR }),
    ).toEqual({ op: "acked", through: CURSOR });
  });

  test("a frame that is not exactly one server frame is rejected", () => {
    const invalid: unknown[] = [
      null,
      {},
      { op: "hello", topic: "global" },
      { op: "hello", topic: "global", subscriber: "not-a-uuid" },
      { op: "status", connection: "online", detail: null },
      { op: "batch", batch_id: "", window: WINDOW, lapsed: null, events: [] },
      { op: "batch", batch_id: "b", window: WINDOW, events: [] },
      { op: "published", request_id: "", id: EVENT_ID, seq: 0, at: AT },
      { op: "published", request_id: "r", id: "nope", seq: 0, at: AT },
      { op: "publish_failed", request_id: "r", detail: "x" },
      { op: "acked" },
      { op: "unknown" },
    ];

    for (const frame of invalid) {
      expect(Bridge.decodeBridgeServerFrame(frame)).toBeNull();
    }
  });

  test("a server frame parsed from text is decoded and malformed text is not", () => {
    expect(
      Bridge.parseBridgeServerFrame(
        JSON.stringify({ op: "hello", topic: "global", subscriber: SUBSCRIBER_ID }),
      ),
    ).toEqual({ op: "hello", topic: "global", subscriber: SUBSCRIBER_ID });
    expect(Bridge.parseBridgeServerFrame("{")).toBeNull();
    expect(Bridge.parseBridgeServerFrame("null")).toBeNull();
    expect(Bridge.parseBridgeServerFrame(JSON.stringify({ op: "hello" }))).toBeNull();
  });
});

describe("the frames a browser sends to the bridge", () => {
  test("an ack names its batch and the cursor it covers", () => {
    expect(
      Bridge.decodeBridgeClientFrame({ op: "ack", batch_id: "batch-1", through: CURSOR }),
    ).toEqual({ op: "ack", batch_id: "batch-1", through: CURSOR });
  });

  test("a publish names its request and carries any JSON payload", () => {
    for (const payload of [{ turn: 7 }, [1], "text", 0, true, null]) {
      expect(
        Bridge.decodeBridgeClientFrame({ op: "publish", request_id: "request-1", payload }),
      ).toEqual({ op: "publish", request_id: "request-1", payload });
    }
  });

  test("a frame that is not exactly one client frame is rejected", () => {
    const invalid: unknown[] = [
      null,
      {},
      { op: "ack", batch_id: "", through: CURSOR },
      { op: "ack", through: CURSOR },
      { op: "ack", batch_id: "b", through: { epoch: "nope", seq: 1 } },
      { op: "publish", request_id: "", payload: {} },
      { op: "publish", payload: {} },
      { op: "publish", request_id: "r" },
      { op: "pull" },
    ];

    for (const frame of invalid) {
      expect(Bridge.decodeBridgeClientFrame(frame)).toBeNull();
    }
  });

  test("a client frame parsed from text is decoded and malformed text is not", () => {
    expect(
      Bridge.parseBridgeClientFrame(
        JSON.stringify({ op: "ack", batch_id: "batch-1", through: CURSOR }),
      ),
    ).toEqual({ op: "ack", batch_id: "batch-1", through: CURSOR });
    expect(Bridge.parseBridgeClientFrame("{")).toBeNull();
    expect(Bridge.parseBridgeClientFrame("null")).toBeNull();
    expect(Bridge.parseBridgeClientFrame(JSON.stringify({ op: "ack" }))).toBeNull();
  });

  test("a client frame encodes to the bridge grammar", () => {
    expect(
      Bridge.encodeBridgeClientFrame({
        op: "publish",
        request_id: "request-1",
        payload: { turn: 7 },
      }),
    ).toBe('{"op":"publish","request_id":"request-1","payload":{"turn":7}}');
    expect(
      Bridge.encodeBridgeClientFrame({ op: "ack", batch_id: "batch-1", through: CURSOR }),
    ).toBe(`{"op":"ack","batch_id":"batch-1","through":{"epoch":"${EPOCH}","seq":1}}`);
  });

  test("each connection phase reads as a sentence", () => {
    expect(Bridge.describeConnection("connecting")).toBe("Connecting to the bridge");
    expect(Bridge.describeConnection("live")).toBe("Streaming from the bus");
    expect(Bridge.describeConnection("refused")).toBe("The bus refused the subscription");
    expect(Bridge.describeConnection("offline")).toBe("The bridge is offline");
  });
});

describe("the bridge health document", () => {
  test("health names the subscription and its delivery counters", () => {
    const health = {
      status: "live",
      topic: "global",
      subscriber: SUBSCRIBER_ID,
      socket: "/run/user/1000/mokmok/bus.sock",
      events_delivered: 42,
      last_seq: 41,
      pending_batch: null,
      connected_at_ms: 1_792_406_045_029,
    };

    expect(Bridge.decodeHealthResponse(health)).toEqual(health);
    expect(
      Bridge.decodeHealthResponse({ ...health, last_seq: null, pending_batch: "batch-1" }),
    ).toEqual({ ...health, last_seq: null, pending_batch: "batch-1" });
  });

  test("an invalid health document is rejected", () => {
    const health = {
      status: "live",
      topic: "global",
      subscriber: SUBSCRIBER_ID,
      socket: "/tmp/bus.sock",
      events_delivered: 0,
      last_seq: null,
      pending_batch: null,
      connected_at_ms: null,
    };

    for (const invalid of [
      null,
      {},
      { ...health, status: "online" },
      { ...health, subscriber: "nope" },
      { ...health, events_delivered: -1 },
      { ...health, events_delivered: 1.5 },
      { ...health, last_seq: -1 },
      { ...health, connected_at_ms: -1 },
      { ...health, pending_batch: 7 },
    ]) {
      expect(Bridge.decodeHealthResponse(invalid)).toBeNull();
    }
  });

  test("health parsed from text is decoded and malformed text is not", () => {
    const health = {
      status: "offline",
      topic: "global",
      subscriber: SUBSCRIBER_ID,
      socket: "/tmp/bus.sock",
      events_delivered: 0,
      last_seq: null,
      pending_batch: null,
      connected_at_ms: null,
    };

    expect(Bridge.parseHealthResponse(JSON.stringify(health))).toEqual(health);
    expect(Bridge.parseHealthResponse("{")).toBeNull();
    expect(Bridge.parseHealthResponse("null")).toBeNull();
  });
});
