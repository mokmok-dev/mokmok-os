import { describe, expect, test } from "bun:test";

import {
  parsePlan,
  type PlanInput,
  stepAt,
  type StepSpec,
} from "../src/plan.ts";

const plain = (name: string): StepSpec => ({ name, approval: "none" });
const approval = (name: string): StepSpec => ({ name, approval: "required" });
const input = (steps: readonly StepSpec[]): PlanInput => ({
  id: "plan",
  version: 1,
  steps,
});

describe("parsePlan", () => {
  test("accepts a non-empty plan and keeps its fields", () => {
    expect(parsePlan(input([plain("a"), approval("b")]))).toEqual({
      kind: "parsed",
      plan: { id: "plan", version: 1, steps: [plain("a"), approval("b")] },
    });
  });

  test("rejects an empty plan", () => {
    expect(parsePlan(input([]))).toEqual({
      kind: "invalid",
      error: { kind: "empty-plan" },
    });
  });

  test("rejects a duplicate step name, naming the repeat", () => {
    expect(parsePlan(input([plain("a"), plain("b"), plain("a")]))).toEqual({
      kind: "invalid",
      error: { kind: "duplicate-step", step: "a" },
    });
  });

  test("accepts a single step", () => {
    expect(parsePlan(input([plain("only")]))).toEqual({
      kind: "parsed",
      plan: { id: "plan", version: 1, steps: [plain("only")] },
    });
  });
});

describe("stepAt", () => {
  const plan = { id: "plan", version: 1, steps: [plain("a"), approval("b")] };

  test("returns the step at a valid index", () => {
    expect(stepAt(plan, 0)).toEqual(plain("a"));
    expect(stepAt(plan, 1)).toEqual(approval("b"));
  });

  test("returns undefined past the end", () => {
    expect(stepAt(plan, 2)).toBeUndefined();
  });

  test("returns undefined before the start", () => {
    expect(stepAt(plan, -1)).toBeUndefined();
  });
});
