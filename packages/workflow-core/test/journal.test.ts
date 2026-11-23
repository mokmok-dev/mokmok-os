import { describe, expect, test } from "bun:test";

import { decideReplay, type JournalEntry } from "../src/journal.ts";

const entry = (
  seq: number,
  step: string,
  input: string,
  value: string = step,
): JournalEntry => ({
  seq,
  step,
  input,
  outcome: { status: "applied", value },
});

describe("decideReplay", () => {
  test("executes a call past the recorded prefix", () => {
    expect(decideReplay([], 0, { step: "a", input: "x" })).toEqual({
      kind: "execute",
    });
  });

  test("replays a recorded call whose step and input match", () => {
    expect(
      decideReplay([entry(0, "a", "x")], 0, { step: "a", input: "x" }),
    ).toEqual({ kind: "replay", value: "a" });
  });

  test("rejects a step that differs from the record", () => {
    expect(
      decideReplay([entry(0, "a", "x")], 0, { step: "b", input: "x" }),
    ).toEqual({
      kind: "rejected",
      error: { kind: "replay-divergence", seq: 0, expected: "a", got: "b" },
    });
  });

  test("rejects an input that differs from the record", () => {
    expect(
      decideReplay([entry(0, "a", "x")], 0, { step: "a", input: "y" }),
    ).toEqual({
      kind: "rejected",
      error: { kind: "replay-divergence", seq: 0, expected: "x", got: "y" },
    });
  });

  test("rejects a recorded failure by returning its error", () => {
    const failed: JournalEntry = {
      seq: 0,
      step: "a",
      input: "x",
      outcome: {
        status: "failed",
        error: { kind: "call-failed", step: "a", reason: "boom" },
      },
    };
    expect(decideReplay([failed], 0, { step: "a", input: "x" })).toEqual({
      kind: "rejected",
      error: { kind: "call-failed", step: "a", reason: "boom" },
    });
  });

  test("reads the entry at the issued position", () => {
    const journal = [entry(0, "a", "x"), entry(1, "b", "y")];
    expect(decideReplay(journal, 1, { step: "b", input: "y" })).toEqual({
      kind: "replay",
      value: "b",
    });
    expect(decideReplay(journal, 1, { step: "a", input: "x" })).toEqual({
      kind: "rejected",
      error: { kind: "replay-divergence", seq: 1, expected: "b", got: "a" },
    });
  });
});
