import { describe, expect, test } from "vitest";

import type { Cursor, Event } from "@mokmok/web-protocol";

import type { ConsumerState } from "./state.ts";
import { initialState } from "./state.ts";
import { hydrate, persistable } from "./storage.ts";

const EPOCH = "2904014b-406d-4859-b9d7-657242a21c51";
const OTHER_TOPIC = "session:2904014b-406d-4859-b9d7-657242a21c51";
const TOPIC = "global";
const SUBSCRIBER = "018f2f3e-0000-7000-8000-000000000001";
const AT = { secs_since_epoch: 1_792_406_045, nanos_since_epoch: 29_207_030 };

const eventId = (seq: number): string =>
  `018f2f3e-0000-7000-8000-${String(seq).padStart(12, "0")}`;

const event = (seq: number): Event => ({
  id: eventId(seq),
  topic: TOPIC,
  seq,
  at: AT,
  payload: { seq },
});

const cursor = (seq: number): Cursor => ({ epoch: EPOCH, seq });

const stored = () => ({
  version: 1,
  topic: TOPIC,
  subscriber: SUBSCRIBER,
  checkpoint: cursor(3),
  events: [event(0), event(1), event(2)],
  evicted: 7,
});

const state = (): ConsumerState => ({
  ...initialState(TOPIC),
  phase: "live",
  detail: "streaming",
  subscriber: SUBSCRIBER,
  window: { epoch: EPOCH, first: 0, next: 4 },
  checkpoint: cursor(3),
  pending: { batchId: "batch-1", through: cursor(3) },
  events: [event(0), event(1), event(2)],
  lapsed: { held: cursor(0), from: 1 },
  receipts: [{ requestId: "request-1", id: eventId(2), seq: 2, at: AT }],
  failures: [{ requestId: "request-2", detail: "nope", indeterminate: true }],
  evicted: 7,
});

describe("persistable", () => {
  test("writes a versioned document of the durable fields", () => {
    expect(JSON.parse(persistable(state()))).toEqual(stored());
  });
});

describe("hydrate", () => {
  test("restores the durable fields and resets the live ones", () => {
    expect(hydrate(persistable(state()), TOPIC)).toEqual({
      ...initialState(TOPIC),
      subscriber: SUBSCRIBER,
      checkpoint: cursor(3),
      events: [event(0), event(1), event(2)],
      evicted: 7,
    });
  });

  test("accepts a document without a subscriber", () => {
    const raw = JSON.stringify({ ...stored(), subscriber: null });

    expect(hydrate(raw, TOPIC)).toEqual({
      ...initialState(TOPIC),
      subscriber: null,
      checkpoint: cursor(3),
      events: [event(0), event(1), event(2)],
      evicted: 7,
    });
  });

  test("a missing document opens the initial state", () => {
    expect(hydrate(null, TOPIC)).toEqual(initialState(TOPIC));
  });

  test("a document for another topic opens the initial state", () => {
    const raw = persistable(state());

    expect(hydrate(raw, OTHER_TOPIC)).toEqual(initialState(OTHER_TOPIC));
  });

  test("malformed documents open the initial state", () => {
    const malformed = [
      "",
      "   ",
      "not json",
      "{",
      "null",
      "[]",
      '"text"',
      "42",
      "{}",
      JSON.stringify({ ...stored(), version: 2 }),
      JSON.stringify({ ...stored(), version: "one" }),
      JSON.stringify({ ...stored(), topic: undefined }),
      JSON.stringify({ ...stored(), topic: 7 }),
      JSON.stringify({ ...stored(), subscriber: "not-a-uuid" }),
      JSON.stringify({ ...stored(), checkpoint: { epoch: "nope", seq: 3 } }),
      JSON.stringify({ ...stored(), checkpoint: { epoch: EPOCH } }),
      JSON.stringify({ ...stored(), evicted: -1 }),
      JSON.stringify({ ...stored(), evicted: 1.5 }),
      JSON.stringify({ ...stored(), events: {} }),
      JSON.stringify({
        ...stored(),
        events: [{ id: "nope", topic: TOPIC, seq: 0, at: AT, payload: null }],
      }),
    ];

    for (const raw of malformed) {
      expect(hydrate(raw, TOPIC)).toEqual(initialState(TOPIC));
    }
  });
});
