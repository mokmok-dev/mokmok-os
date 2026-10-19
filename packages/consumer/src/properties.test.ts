import fc from "fast-check";
import { describe, expect, test } from "vitest";

import type { BatchFrame, Cursor, Event } from "@mokmok/web-protocol";

import type { ConsumerMessage, ConsumerState } from "./state.ts";
import { EVENT_CAP, initialState, reduce } from "./state.ts";
import { parsePayload } from "./payload.ts";
import { hydrate, persistable } from "./storage.ts";

const RUN = { seed: 424_242, numRuns: 100 };

const EPOCH = "2904014b-406d-4859-b9d7-657242a21c51";
const OTHER_EPOCH = "2904014b-406d-4859-b9d7-657242a21c52";
const TOPIC = "global";
const AT = { secs_since_epoch: 1_700_000_000, nanos_since_epoch: 0 };

const uuid = (): fc.Arbitrary<string> =>
  fc.uuid().map((value) => value.toLowerCase());

const eventId = (seq: number): string =>
  `018f2f3e-0000-7000-8000-${String(seq).padStart(12, "0")}`;

const event = (seq: number): Event => ({
  id: eventId(seq),
  topic: TOPIC,
  seq,
  at: AT,
  payload: { seq },
});

const epoch = fc.constantFrom(EPOCH, OTHER_EPOCH);

const cursorArb: fc.Arbitrary<Cursor> = fc.record({
  epoch,
  seq: fc.nat({ max: 5000 }),
});

const frameArb: fc.Arbitrary<BatchFrame> = fc
  .record({
    epoch,
    next: fc.nat({ max: 5000 }),
    events: fc.array(fc.nat({ max: 5000 }), { maxLength: 40 }),
    lapsed: fc.option(
      fc.record({ held: cursorArb, from: fc.nat({ max: 5000 }) }),
      { nil: null },
    ),
  })
  .map(({ epoch: frameEpoch, next, events, lapsed }) => ({
    op: "batch" as const,
    batch_id: `batch-${events.length}`,
    window: { epoch: frameEpoch, first: 0, next },
    lapsed,
    events: [...events]
      .sort((left, right) => left - right)
      .map((seq) => event(seq)),
  }));

const atArb = fc.record({
  secs_since_epoch: fc.nat({ max: 2_000_000_000 }),
  nanos_since_epoch: fc.nat({ max: 999_999_999 }),
});

const topicArb = fc.oneof(
  fc.constant(TOPIC),
  uuid().map((value) => `session:${value}`),
);

const stateArb: fc.Arbitrary<ConsumerState> = fc
  .record({
    topic: topicArb,
    subscriber: fc.option(uuid(), { nil: null }),
    checkpoint: fc.option(cursorArb, { nil: null }),
    events: fc.array(
      fc.record({ id: uuid(), seq: fc.nat({ max: 1_000_000 }), at: atArb }),
      { maxLength: 40 },
    ),
    evicted: fc.nat({ max: 100_000 }),
  })
  .map(
    ({ topic, subscriber, checkpoint, events, evicted }): ConsumerState => ({
      ...initialState(topic),
      subscriber,
      checkpoint,
      events: events.map((item) => ({
        id: item.id,
        topic: TOPIC,
        seq: item.seq,
        at: item.at,
        payload: { seq: item.seq },
      })),
      evicted,
    }),
  );

const replay = (
  state: ConsumerState,
  frame: BatchFrame,
): ConsumerState => reduce(state, { type: "batch", frame });

describe("consumer state properties", () => {
  test("replaying a delivered batch is idempotent", () => {
    fc.assert(
      fc.property(
        fc.array(frameArb, { maxLength: 15 }),
        frameArb,
        (frames, duplicate) => {
          const settled = frames.reduce(replay, initialState(TOPIC));
          const once = replay(settled, duplicate);

          expect(replay(once, duplicate)).toEqual(once);
        },
      ),
      RUN,
    );
  });

  test("the checkpoint never moves backwards within an epoch", () => {
    const messageArb: fc.Arbitrary<ConsumerMessage> = fc.oneof(
      frameArb.map((frame): ConsumerMessage => ({ type: "batch", frame })),
      cursorArb.map((through): ConsumerMessage => ({ type: "acked", through })),
    );

    fc.assert(
      fc.property(fc.array(messageArb, { maxLength: 40 }), (messages) => {
        let state = initialState(TOPIC);
        let checkpoint = state.checkpoint;

        for (const message of messages) {
          state = reduce(state, message);

          if (
            checkpoint !== null &&
            state.checkpoint !== null &&
            checkpoint.epoch === state.checkpoint.epoch
          ) {
            expect(state.checkpoint.seq).toBeGreaterThanOrEqual(checkpoint.seq);
          }

          checkpoint = state.checkpoint;
        }
      }),
      RUN,
    );
  });

  test("events stay capped, ordered, and only leave the view by eviction", () => {
    fc.assert(
      fc.property(fc.array(frameArb, { maxLength: 30 }), (frames) => {
        let state = initialState(TOPIC);

        for (const frame of frames) {
          const previous = state;
          state = replay(state, frame);

          expect(state.events.length).toBeLessThanOrEqual(EVENT_CAP);
          expect(state.evicted).toBeGreaterThanOrEqual(0);

          if (
            previous.window !== null &&
            previous.window.epoch === state.window?.epoch
          ) {
            expect(state.evicted).toBeGreaterThanOrEqual(previous.evicted);
          }

          const seqs = state.events.map((item) => item.seq);
          const ordered = seqs.every(
            (seq, index) => index === 0 || seq > (seqs[index - 1] ?? 0),
          );

          expect(ordered).toBe(true);
        }
      }),
      RUN,
    );
  });

  test("persistable and hydrate round-trip the durable state", () => {
    fc.assert(
      fc.property(stateArb, (state) => {
        expect(hydrate(persistable(state), state.topic)).toEqual({
          ...initialState(state.topic),
          subscriber: state.subscriber,
          checkpoint: state.checkpoint,
          events: state.events,
          evicted: state.evicted,
        });
      }),
      RUN,
    );
  });

  test("parsePayload ignores surrounding whitespace", () => {
    const whitespace = fc
      .array(fc.constantFrom(" ", "\t", "\n", "\r"), { maxLength: 8 })
      .map((chars) => chars.join(""));

    fc.assert(
      fc.property(whitespace, fc.jsonValue(), (padding, payload) => {
        expect(
          parsePayload(`${padding}${JSON.stringify(payload)}${padding}`),
        ).toEqual({ ok: true, payload });
      }),
      RUN,
    );
  });
});
