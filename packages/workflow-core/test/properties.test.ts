import { describe, expect, test } from "bun:test";

import fc from "fast-check";

import { decideReplay } from "../src/journal.ts";
import type { JsonValue, Plan, StepSpec } from "../src/plan.ts";
import {
  expectedStep,
  isTerminal,
  phaseName,
  reduce,
  type RunState,
  startRun,
} from "../src/run.ts";

const nameArb = fc.constantFrom("a", "b", "c", "d");
const stepArb: fc.Arbitrary<StepSpec> = fc.record({
  name: nameArb,
  approval: fc.constantFrom("none" as const, "required" as const),
});

/** Unique step names, so the plan is always valid. */
const planArb: fc.Arbitrary<Plan> = fc
  .uniqueArray(stepArb, { selector: (s) => s.name, minLength: 1, maxLength: 4 })
  .map((steps) => ({ id: "plan", version: 1, steps }));

const valueArb: fc.Arbitrary<JsonValue> = fc.oneof(
  fc.string(),
  fc.integer(),
  fc.boolean(),
  fc.constant(null),
  fc.array(fc.integer(), { maxLength: 3 }),
);

function begun(plan: Plan): RunState {
  const outcome = startRun(plan);
  if (outcome.kind !== "started") {
    throw new Error(`plan was refused: ${JSON.stringify(outcome.error)}`);
  }
  return outcome.run;
}

function advance(run: RunState, value: JsonValue): RunState {
  const name = expectedStep(run);
  if (name === undefined) {
    return run;
  }
  const outcome = reduce(run, {
    type: "applied",
    call: { step: name, input: "in" },
    value,
  });
  return outcome.kind === "reduced" ? outcome.run : run;
}

describe("run properties", () => {
  test("any sequence of events never throws and always lands in a legal phase", () => {
    fc.assert(
      fc.property(
        planArb,
        fc.array(
          fc.oneof(
            fc.record({ type: fc.constant("pause" as const), step: nameArb }),
            fc.record({ type: fc.constant("resume" as const) }),
            fc.record({
              type: fc.constant("applied" as const),
              call: fc.record({ step: nameArb, input: fc.string() }),
              value: valueArb,
            }),
            fc.record({
              type: fc.constant("fail" as const),
              error: fc.constant({ kind: "empty-plan" as const }),
            }),
            fc.record({ type: fc.constant("rollback" as const) }),
          ),
          { maxLength: 12 },
        ),
        (plan, events) => {
          let run = begun(plan);
          for (const event of events) {
            const outcome = reduce(run, event);
            if (outcome.kind === "reduced") {
              run = outcome.run;
            }
            expect([
              "running",
              "paused",
              "completed",
              "failed",
              "rolled-back",
            ]).toContain(phaseName(run));
          }
          return true;
        },
      ),
      { numRuns: 200 },
    );
  });

  test("the journal is always a prefix of the plan", () => {
    fc.assert(
      fc.property(
        planArb,
        fc.array(valueArb, { minLength: 1, maxLength: 6 }),
        (plan, values) => {
          let run = begun(plan);
          for (const value of values) {
            run = advance(run, value);
            for (const [index, entry] of run.journal.entries()) {
              const spec = plan.steps[index];
              if (
                spec === undefined ||
                spec.name !== entry.step ||
                entry.seq !== index
              ) {
                return false;
              }
            }
            if (run.journal.length > plan.steps.length) {
              return false;
            }
          }
          return true;
        },
      ),
      { numRuns: 200 },
    );
  });

  test("advancing to the last step terminates the run and no further event revives it", () => {
    fc.assert(
      fc.property(
        planArb,
        fc.array(valueArb, { minLength: 1, maxLength: 6 }),
        (plan, values) => {
          let run = begun(plan);
          for (const value of values) {
            run = advance(run, value);
          }
          if (run.journal.length !== plan.steps.length) {
            return true;
          }
          if (
            !isTerminal(run) ||
            phaseName(run) !== "completed" ||
            expectedStep(run) !== undefined
          ) {
            return false;
          }
          const refused = reduce(run, {
            type: "applied",
            call: { step: "a", input: "in" },
            value: null,
          });
          return (
            refused.kind === "refused" && refused.error.kind === "not-running"
          );
        },
      ),
      { numRuns: 200 },
    );
  });

  test("replaying a finished journal yields the recorded value and never re-executes", () => {
    fc.assert(
      fc.property(
        planArb,
        fc.array(valueArb, { minLength: 1, maxLength: 6 }),
        (plan, values) => {
          let run = begun(plan);
          for (const value of values) {
            run = advance(run, value);
          }
          return run.journal.every((entry, issued) => {
            const decision = decideReplay(run.journal, issued, {
              step: entry.step,
              input: entry.input,
            });
            return (
              decision.kind === "replay" &&
              entry.outcome.status === "applied" &&
              JSON.stringify(decision.value) ===
                JSON.stringify(entry.outcome.value)
            );
          });
        },
      ),
      { numRuns: 200 },
    );
  });

  test("an empty plan is always refused", () => {
    fc.assert(
      fc.property(fc.string(), fc.nat(), (id, version) => {
        const outcome = startRun({ id, version, steps: [] });
        return (
          outcome.kind === "refused" && outcome.error.kind === "empty-plan"
        );
      }),
      { numRuns: 50 },
    );
  });

  test("a paused run accepts resume, rollback, and a failure, and refuses progress", () => {
    fc.assert(
      fc.property(planArb, (plan) => {
        const running = begun(plan);
        const first = plan.steps[0];
        if (first === undefined) {
          return true;
        }
        const paused = reduce(running, { type: "pause", step: first.name });
        if (paused.kind !== "reduced") {
          return false;
        }
        return (
          reduce(paused.run, { type: "resume" }).kind === "reduced" &&
          reduce(paused.run, { type: "rollback" }).kind === "reduced" &&
          reduce(paused.run, { type: "fail", error: { kind: "not-paused" } })
            .kind === "reduced" &&
          reduce(paused.run, {
            type: "applied",
            call: { step: first.name, input: "in" },
            value: null,
          }).kind === "refused"
        );
      }),
      { numRuns: 100 },
    );
  });
});
