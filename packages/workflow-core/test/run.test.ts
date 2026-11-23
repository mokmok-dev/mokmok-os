import { describe, expect, test } from "bun:test";

import type { Plan } from "../src/plan.ts";
import {
  expectedStep,
  isTerminal,
  phaseName,
  reduce,
  type RunEvent,
  type RunOutcome,
  type RunState,
  startRun,
} from "../src/run.ts";

const plain = (name: string) => ({ name, approval: "none" as const });
const twoStep: Plan = {
  id: "plan",
  version: 1,
  steps: [plain("a"), plain("b")],
};
const oneStep: Plan = { id: "plan", version: 1, steps: [plain("a")] };
const call = (step: string, input = "x") => ({ step, input });

function begun(plan: Plan): RunState {
  const outcome = startRun(plan);
  if (outcome.kind !== "started") {
    throw new Error(`unexpected refusal: ${JSON.stringify(outcome.error)}`);
  }
  return outcome.run;
}

function step(run: RunState, event: RunEvent): RunState {
  const outcome = reduce(run, event);
  if (outcome.kind !== "reduced") {
    throw new Error(`unexpected refusal: ${JSON.stringify(outcome.error)}`);
  }
  return outcome.run;
}

describe("startRun", () => {
  test("starts running on the first step with an empty journal", () => {
    expect(startRun(twoStep)).toEqual({
      kind: "started",
      run: {
        plan: twoStep,
        phase: { phase: "running", next: plain("a") },
        journal: [],
      },
    });
  });

  test("refuses an empty plan", () => {
    expect(startRun({ id: "plan", version: 1, steps: [] })).toEqual({
      kind: "refused",
      error: { kind: "empty-plan" },
    });
  });
});

describe("expectedStep", () => {
  test("names the next step from the phase", () => {
    expect(expectedStep(begun(twoStep))).toBe("a");
  });

  test("advances as the journal grows", () => {
    expect(
      expectedStep(
        step(begun(twoStep), { type: "applied", call: call("a"), value: 1 }),
      ),
    ).toBe("b");
  });

  test("keeps naming the step while paused", () => {
    expect(
      expectedStep(step(begun(twoStep), { type: "pause", step: "a" })),
    ).toBe("a");
  });

  test("is undefined once the plan is done", () => {
    expect(
      expectedStep(
        step(begun(oneStep), { type: "applied", call: call("a"), value: 1 }),
      ),
    ).toBeUndefined();
  });

  test("is undefined when failed and when rolled back", () => {
    expect(
      expectedStep(
        step(begun(twoStep), { type: "fail", error: { kind: "empty-plan" } }),
      ),
    ).toBeUndefined();
    expect(
      expectedStep(step(begun(twoStep), { type: "rollback" })),
    ).toBeUndefined();
  });
});

describe("isTerminal", () => {
  const running = begun(twoStep);

  test("is false while running", () => {
    expect(isTerminal(running)).toBe(false);
  });

  test("is false while paused", () => {
    expect(isTerminal(step(running, { type: "pause", step: "a" }))).toBe(false);
  });

  test("is true once the last step is applied", () => {
    const done = step(running, { type: "applied", call: call("a"), value: 1 });
    expect(isTerminal(done)).toBe(false);
    const finished = step(done, { type: "applied", call: call("b"), value: 2 });
    expect(isTerminal(finished)).toBe(true);
    expect(phaseName(finished)).toBe("completed");
  });

  test("is true when failed", () => {
    const failed = step(running, {
      type: "fail",
      error: { kind: "empty-plan" },
    });
    expect(isTerminal(failed)).toBe(true);
    expect(phaseName(failed)).toBe("failed");
  });

  test("is true when rolled back", () => {
    const rolled = step(running, { type: "rollback" });
    expect(isTerminal(rolled)).toBe(true);
    expect(phaseName(rolled)).toBe("rolled-back");
  });
});

describe("reduce", () => {
  test("pause records the step and keeps the journal", () => {
    expect(reduce(begun(twoStep), { type: "pause", step: "a" })).toEqual({
      kind: "reduced",
      run: {
        plan: twoStep,
        phase: { phase: "paused", step: "a", next: plain("a") },
        journal: [],
      },
    });
  });

  test("pause refuses a step out of order", () => {
    expect(reduce(begun(twoStep), { type: "pause", step: "b" })).toEqual({
      kind: "refused",
      error: { kind: "out-of-order", expected: "a", got: "b" },
    });
  });

  test("pause refuses a run that is already paused", () => {
    const paused = step(begun(twoStep), { type: "pause", step: "a" });
    expect(reduce(paused, { type: "pause", step: "a" })).toEqual({
      kind: "refused",
      error: { kind: "not-running", phase: "paused" },
    });
  });

  test("resume returns to running on the same step", () => {
    const paused = step(begun(twoStep), { type: "pause", step: "a" });
    expect(reduce(paused, { type: "resume" })).toEqual({
      kind: "reduced",
      run: {
        plan: twoStep,
        phase: { phase: "running", next: plain("a") },
        journal: [],
      },
    });
  });

  test("resume refuses a run that is not paused", () => {
    expect(reduce(begun(twoStep), { type: "resume" })).toEqual({
      kind: "refused",
      error: { kind: "not-paused" },
    });
  });

  test("applied journals the call and stays running before the last step", () => {
    expect(
      reduce(begun(twoStep), { type: "applied", call: call("a"), value: 10 }),
    ).toEqual({
      kind: "reduced",
      run: {
        plan: twoStep,
        phase: { phase: "running", next: plain("b") },
        journal: [
          {
            seq: 0,
            step: "a",
            input: "x",
            outcome: { status: "applied", value: 10 },
          },
        ],
      },
    });
  });

  test("applied completes the run on the last step", () => {
    const first = step(begun(twoStep), {
      type: "applied",
      call: call("a"),
      value: 10,
    });
    expect(
      reduce(first, { type: "applied", call: call("b"), value: 20 }),
    ).toEqual({
      kind: "reduced",
      run: {
        plan: twoStep,
        phase: { phase: "completed", value: 20 },
        journal: [
          {
            seq: 0,
            step: "a",
            input: "x",
            outcome: { status: "applied", value: 10 },
          },
          {
            seq: 1,
            step: "b",
            input: "x",
            outcome: { status: "applied", value: 20 },
          },
        ],
      },
    });
  });

  test("applied refuses a step out of order", () => {
    expect(
      reduce(begun(twoStep), { type: "applied", call: call("b"), value: 1 }),
    ).toEqual({
      kind: "refused",
      error: { kind: "out-of-order", expected: "a", got: "b" },
    });
  });

  test("applied refuses a run that is paused", () => {
    const paused = step(begun(twoStep), { type: "pause", step: "a" });
    expect(
      reduce(paused, { type: "applied", call: call("a"), value: 1 }),
    ).toEqual({
      kind: "refused",
      error: { kind: "not-running", phase: "paused" },
    });
  });

  test("fail records the error and keeps the journal", () => {
    const first = step(begun(twoStep), {
      type: "applied",
      call: call("a"),
      value: 10,
    });
    expect(
      reduce(first, { type: "fail", error: { kind: "empty-plan" } }),
    ).toEqual({
      kind: "reduced",
      run: {
        plan: twoStep,
        phase: { phase: "failed", error: { kind: "empty-plan" } },
        journal: [
          {
            seq: 0,
            step: "a",
            input: "x",
            outcome: { status: "applied", value: 10 },
          },
        ],
      },
    });
  });

  test("rollback is allowed from any phase and keeps the journal", () => {
    const first = step(begun(twoStep), {
      type: "applied",
      call: call("a"),
      value: 10,
    });
    expect(reduce(first, { type: "rollback" })).toEqual({
      kind: "reduced",
      run: {
        plan: twoStep,
        phase: { phase: "rolled-back" },
        journal: [
          {
            seq: 0,
            step: "a",
            input: "x",
            outcome: { status: "applied", value: 10 },
          },
        ],
      },
    });
  });

  test("rollback twice stays rolled back", () => {
    const rolled = step(begun(twoStep), { type: "rollback" });
    expect(reduce(rolled, { type: "rollback" })).toEqual({
      kind: "reduced",
      run: { plan: twoStep, phase: { phase: "rolled-back" }, journal: [] },
    });
  });

  test("every other event is refused once the run is terminal", () => {
    const failed = step(begun(twoStep), {
      type: "fail",
      error: { kind: "empty-plan" },
    });
    const refusal: RunOutcome = {
      kind: "refused",
      error: { kind: "not-running", phase: "failed" },
    };
    expect(reduce(failed, { type: "pause", step: "a" })).toEqual(refusal);
    expect(reduce(failed, { type: "resume" })).toEqual(refusal);
    expect(
      reduce(failed, { type: "applied", call: call("a"), value: 1 }),
    ).toEqual(refusal);
    expect(
      reduce(failed, { type: "fail", error: { kind: "not-paused" } }),
    ).toEqual(refusal);
  });
});
