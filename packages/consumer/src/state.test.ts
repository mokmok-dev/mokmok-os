import { describe, expect, test } from "vitest";

import type {
  BatchFrame,
  Cursor,
  Event,
  PublishedFrame,
  PublishFailedFrame,
} from "@mokmok/web-protocol";

import type { ConsumerMessage, ConsumerState } from "./state.ts";
import { EVENT_CAP, batchThrough, initialState, reduce } from "./state.ts";

const EPOCH = "2904014b-406d-4859-b9d7-657242a21c51";
const OTHER_EPOCH = "2904014b-406d-4859-b9d7-657242a21c52";
const TOPIC = "global";
const SUBSCRIBER = "018f2f3e-0000-7000-8000-000000000001";
const OTHER_SUBSCRIBER = "018f2f3e-0000-7000-8000-000000000002";
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

const window = (epoch: string, next: number) => ({ epoch, first: 0, next });

const cursor = (epoch: string, seq: number): Cursor => ({ epoch, seq });

interface FrameOptions {
  batchId?: string;
  epoch?: string;
  next?: number;
  lapsed?: BatchFrame["lapsed"];
  events?: Event[];
}

const frame = (options: FrameOptions = {}): BatchFrame => ({
  op: "batch",
  batch_id: options.batchId ?? "batch-1",
  window: window(options.epoch ?? EPOCH, options.next ?? 0),
  lapsed: options.lapsed ?? null,
  events: options.events ?? [],
});

const live = (): ConsumerState =>
  reduce(initialState(TOPIC), {
    type: "hello",
    topic: TOPIC,
    subscriber: SUBSCRIBER,
  });

const send = (state: ConsumerState, message: ConsumerMessage): ConsumerState =>
  reduce(state, message);

const batch = (state: ConsumerState, options: FrameOptions): ConsumerState =>
  reduce(state, { type: "batch", frame: frame(options) });

const receipt = (requestId: string, seq: number): PublishedFrame => ({
  op: "published",
  request_id: requestId,
  id: eventId(seq),
  seq,
  at: AT,
});

const failure = (
  requestId: string,
  indeterminate: boolean,
): PublishFailedFrame => ({
  op: "publish_failed",
  request_id: requestId,
  detail: `failed ${requestId}`,
  indeterminate,
});

describe("the initial consumer state", () => {
  test("opens empty and connecting", () => {
    expect(initialState(TOPIC)).toEqual({
      topic: TOPIC,
      phase: "connecting",
      detail: null,
      subscriber: null,
      window: null,
      checkpoint: null,
      pending: null,
      events: [],
      lapsed: null,
      receipts: [],
      failures: [],
      evicted: 0,
    });
  });
});

describe("the connection messages", () => {
  test("hello records the topic and subscriber without disturbing the rest", () => {
    const opened = send(live(), {
      type: "hello",
      topic: `session:${EPOCH}`,
      subscriber: OTHER_SUBSCRIBER,
    });

    expect(opened).toEqual({
      ...initialState(TOPIC),
      topic: `session:${EPOCH}`,
      subscriber: OTHER_SUBSCRIBER,
    });
  });

  test("status replaces the phase and detail", () => {
    const connecting = initialState(TOPIC);
    const streaming = send(connecting, {
      type: "status",
      connection: "live",
      detail: null,
    });

    expect(streaming.phase).toBe("live");
    expect(streaming.detail).toBeNull();
    expect(streaming.topic).toBe(TOPIC);

    const offline = send(streaming, {
      type: "status",
      connection: "offline",
      detail: "bridge socket closed",
    });

    expect(offline.phase).toBe("offline");
    expect(offline.detail).toBe("bridge socket closed");
  });
});

describe("batch application", () => {
  test("an empty batch moves the window and the pending position", () => {
    const delivered = batch(live(), { next: 7 });

    expect(delivered.window).toEqual(window(EPOCH, 7));
    expect(delivered.pending).toEqual({
      batchId: "batch-1",
      through: cursor(EPOCH, 7),
    });
    expect(delivered.events).toEqual([]);
    expect(delivered.lapsed).toBeNull();
    expect(delivered.evicted).toBe(0);
  });

  test("a batch with events applies them oldest first and pends the next position", () => {
    const delivered = batch(live(), {
      next: 3,
      events: [event(0), event(1)],
    });

    expect(delivered.events).toEqual([event(0), event(1)]);
    expect(delivered.pending).toEqual({
      batchId: "batch-1",
      through: cursor(EPOCH, 2),
    });
    expect(delivered.evicted).toBe(0);
  });

  test("a lapsed batch records the lapse and pends the restored position", () => {
    const lapsed = { held: cursor(EPOCH, 4), from: 9 };
    const delivered = batch(live(), { next: 12, lapsed });

    expect(delivered.lapsed).toEqual(lapsed);
    expect(delivered.events).toEqual([]);
    expect(delivered.pending).toEqual({
      batchId: "batch-1",
      through: cursor(EPOCH, 9),
    });
  });

  test("events below the high water mark are dropped", () => {
    const first = batch(live(), {
      next: 3,
      events: [event(0), event(1), event(2)],
    });
    const overlapping = batch(first, {
      batchId: "batch-2",
      next: 4,
      events: [event(1), event(2), event(3)],
    });

    expect(overlapping.events).toEqual([event(0), event(1), event(2), event(3)]);
    expect(overlapping.evicted).toBe(0);
  });

  test("replaying a delivered batch is idempotent", () => {
    const applied = batch(live(), {
      next: 2,
      events: [event(0), event(1)],
    });
    const replayed = batch(applied, {
      next: 2,
      events: [event(0), event(1)],
    });

    expect(replayed).toEqual(applied);
  });

  test("duplicates inside one batch are applied once", () => {
    const delivered = batch(live(), {
      next: 3,
      events: [event(1), event(1), event(2), event(2)],
    });

    expect(delivered.events).toEqual([event(1), event(2)]);
  });

  test("gaps after the high water mark are still applied", () => {
    const delivered = batch(live(), {
      next: 4,
      events: [event(0), event(2), event(3)],
    });

    expect(delivered.events).toEqual([event(0), event(2), event(3)]);
  });

  test("a new epoch resets the events, the window, and the lapse", () => {
    const old = batch(live(), {
      next: 3,
      lapsed: { held: cursor(EPOCH, 1), from: 2 },
      events: [event(0), event(1), event(2)],
    });
    const fresh = batch(old, {
      batchId: "batch-2",
      epoch: OTHER_EPOCH,
      next: 1,
      events: [event(0)],
    });

    expect(fresh.events).toEqual([event(0)]);
    expect(fresh.window).toEqual(window(OTHER_EPOCH, 1));
    expect(fresh.lapsed).toBeNull();
    expect(fresh.pending).toEqual({
      batchId: "batch-2",
      through: cursor(OTHER_EPOCH, 1),
    });
  });

  test("a new epoch restarts the sequences", () => {
    const old = batch(live(), { next: 6, events: [event(4), event(5)] });
    const fresh = batch(old, {
      batchId: "batch-2",
      epoch: OTHER_EPOCH,
      next: 1,
      events: [event(0)],
    });

    expect(fresh.events).toEqual([event(0)]);
  });

  test("the event list stops at the cap and counts what it dropped", () => {
    const full = Array.from({ length: EVENT_CAP }, (_, index) => event(index));
    const atCap = batch(live(), { next: EVENT_CAP, events: full });

    expect(atCap.events).toHaveLength(EVENT_CAP);
    expect(atCap.events[0]).toEqual(event(0));
    expect(atCap.events[EVENT_CAP - 1]).toEqual(event(EVENT_CAP - 1));
    expect(atCap.evicted).toBe(0);

    const overflowed = batch(live(), {
      next: EVENT_CAP + 1,
      events: [...full, event(EVENT_CAP)],
    });

    expect(overflowed.events).toHaveLength(EVENT_CAP);
    expect(overflowed.events[0]).toEqual(event(1));
    expect(overflowed.events[EVENT_CAP - 1]).toEqual(event(EVENT_CAP));
    expect(overflowed.evicted).toBe(1);
  });

  test("later batches keep evicting from the front", () => {
    const full = Array.from({ length: EVENT_CAP }, (_, index) => event(index));
    const capped = batch(live(), { next: EVENT_CAP, events: full });
    const appended = batch(capped, {
      batchId: "batch-2",
      next: EVENT_CAP + 2,
      events: [event(EVENT_CAP), event(EVENT_CAP + 1)],
    });

    expect(appended.events).toHaveLength(EVENT_CAP);
    expect(appended.events[0]).toEqual(event(2));
    expect(appended.events[EVENT_CAP - 1]).toEqual(event(EVENT_CAP + 1));
    expect(appended.evicted).toBe(2);
  });

  test("a new epoch clears the eviction count", () => {
    const full = Array.from({ length: EVENT_CAP + 1 }, (_, index) => event(index));
    const overflowed = batch(live(), { next: EVENT_CAP + 1, events: full });

    expect(overflowed.evicted).toBe(1);

    const fresh = batch(overflowed, {
      batchId: "batch-2",
      epoch: OTHER_EPOCH,
      next: 1,
      events: [event(0)],
    });

    expect(fresh.evicted).toBe(0);
    expect(fresh.events).toEqual([event(0)]);
  });
});

describe("batchThrough", () => {
  test("pends one past the last event in the frame's epoch", () => {
    expect(
      batchThrough(
        frame({ epoch: OTHER_EPOCH, next: 12, events: [event(4), event(6)] }),
      ),
    ).toEqual(cursor(OTHER_EPOCH, 7));
  });

  test("pends the lapse position when the batch holds no events", () => {
    expect(
      batchThrough(
        frame({
          epoch: OTHER_EPOCH,
          next: 12,
          lapsed: { held: cursor(EPOCH, 3), from: 8 },
        }),
      ),
    ).toEqual(cursor(OTHER_EPOCH, 8));
  });

  test("pends the window position when the batch is empty and not lapsed", () => {
    expect(batchThrough(frame({ epoch: OTHER_EPOCH, next: 12 }))).toEqual(
      cursor(OTHER_EPOCH, 12),
    );
  });
});

describe("acks", () => {
  test("a matching ack clears pending and advances the checkpoint", () => {
    const awaiting = batch(live(), { next: 3, events: [event(0), event(1)] });
    const acked = send(awaiting, { type: "acked", through: cursor(EPOCH, 2) });

    expect(acked.checkpoint).toEqual(cursor(EPOCH, 2));
    expect(acked.pending).toBeNull();
    expect(acked.events).toEqual([event(0), event(1)]);
  });

  test("an ack with no matching pending batch changes nothing", () => {
    const awaiting = batch(live(), { next: 3, events: [event(0), event(1)] });

    expect(
      send(awaiting, { type: "acked", through: cursor(EPOCH, 1) }),
    ).toBe(awaiting);
    expect(
      send(awaiting, { type: "acked", through: cursor(OTHER_EPOCH, 2) }),
    ).toBe(awaiting);

    const idle = live();

    expect(send(idle, { type: "acked", through: cursor(EPOCH, 2) })).toBe(idle);
  });

  test("the checkpoint never moves backwards", () => {
    const first = batch(live(), {
      next: 6,
      events: [event(0), event(1), event(2), event(3), event(4)],
    });
    const settled = send(first, { type: "acked", through: cursor(EPOCH, 5) });

    expect(settled.checkpoint).toEqual(cursor(EPOCH, 5));

    const stale = batch(settled, {
      batchId: "stale",
      next: 3,
      events: [event(0), event(1)],
    });
    const afterStaleAck = send(stale, {
      type: "acked",
      through: cursor(EPOCH, 2),
    });

    expect(afterStaleAck.checkpoint).toEqual(cursor(EPOCH, 5));
    expect(afterStaleAck.pending).toBeNull();
  });

  test("a checkpoint follows the log into a new epoch", () => {
    const first = batch(live(), {
      next: 6,
      events: [event(0), event(1), event(2), event(3), event(4)],
    });
    const settled = send(first, { type: "acked", through: cursor(EPOCH, 5) });
    const fresh = batch(settled, {
      batchId: "batch-2",
      epoch: OTHER_EPOCH,
      next: 2,
      events: [event(0)],
    });
    const afterNewEpochAck = send(fresh, {
      type: "acked",
      through: cursor(OTHER_EPOCH, 1),
    });

    expect(afterNewEpochAck.checkpoint).toEqual(cursor(OTHER_EPOCH, 1));
  });
});

describe("publish receipts", () => {
  test("a receipt is prepended with its request identity", () => {
    const one = send(live(), {
      type: "published",
      frame: receipt("request-1", 3),
    });

    expect(one.receipts).toEqual([
      { requestId: "request-1", id: eventId(3), seq: 3, at: AT },
    ]);

    const two = send(one, {
      type: "published",
      frame: receipt("request-2", 4),
    });

    expect(two.receipts).toEqual([
      { requestId: "request-2", id: eventId(4), seq: 4, at: AT },
      { requestId: "request-1", id: eventId(3), seq: 3, at: AT },
    ]);
  });

  test("the receipt list is capped and keeps the newest", () => {
    let state = live();

    for (let index = 0; index < 101; index += 1) {
      state = send(state, {
        type: "published",
        frame: receipt(`request-${index}`, index),
      });
    }

    expect(state.receipts).toHaveLength(100);
    expect(state.receipts[0]).toEqual({
      requestId: "request-100",
      id: eventId(100),
      seq: 100,
      at: AT,
    });
    expect(state.receipts[99]).toEqual({
      requestId: "request-1",
      id: eventId(1),
      seq: 1,
      at: AT,
    });
  });
});

describe("publish failures", () => {
  test("a failure is prepended with its detail and indeterminate flag", () => {
    const one = send(live(), {
      type: "publish_failed",
      frame: failure("request-1", true),
    });
    const two = send(one, {
      type: "publish_failed",
      frame: failure("request-2", false),
    });

    expect(two.failures).toEqual([
      {
        requestId: "request-2",
        detail: "failed request-2",
        indeterminate: false,
      },
      {
        requestId: "request-1",
        detail: "failed request-1",
        indeterminate: true,
      },
    ]);
  });

  test("the failure list is capped and keeps the newest", () => {
    let state = live();

    for (let index = 0; index < 101; index += 1) {
      state = send(state, {
        type: "publish_failed",
        frame: failure(`request-${index}`, false),
      });
    }

    expect(state.failures).toHaveLength(100);
    expect(state.failures[0]?.requestId).toBe("request-100");
    expect(state.failures[99]?.requestId).toBe("request-1");
  });
});

describe("clear_events", () => {
  test("keeps the topic, subscriber, window, and checkpoint but empties the view", () => {
    const full = Array.from({ length: EVENT_CAP + 1 }, (_, index) => event(index));
    const overflowed = batch(live(), {
      next: EVENT_CAP + 1,
      events: full,
    });
    const settled = send(overflowed, {
      type: "acked",
      through: cursor(EPOCH, EVENT_CAP + 1),
    });
    const cleared = send(settled, { type: "clear_events" });

    expect(cleared).toEqual({
      ...settled,
      events: [],
      evicted: 0,
    });
    expect(cleared.topic).toBe(TOPIC);
    expect(cleared.subscriber).toBe(SUBSCRIBER);
    expect(cleared.checkpoint).toEqual(cursor(EPOCH, EVENT_CAP + 1));
    expect(cleared.window).toEqual(window(EPOCH, EVENT_CAP + 1));
  });
});
