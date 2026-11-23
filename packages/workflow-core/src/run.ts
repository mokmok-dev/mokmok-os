import type { WorkflowError } from "./error.ts";
import type { Call, JournalEntry } from "./journal.ts";
import type { JsonValue, Plan, StepSpec } from "./plan.ts";
import { stepAt } from "./plan.ts";

/**
 * `running` and `paused` carry the step they are about to run. A run therefore
 * cannot exist without a next step, and "the plan is exhausted" becomes the
 * `completed` phase instead of a branch every transition repeats.
 */
export type RunPhase =
  | { readonly phase: "running"; readonly next: StepSpec }
  | { readonly phase: "paused"; readonly step: string; readonly next: StepSpec }
  | { readonly phase: "completed"; readonly value: JsonValue }
  | { readonly phase: "failed"; readonly error: WorkflowError }
  | { readonly phase: "rolled-back" };

export type RunState = {
  readonly plan: Plan;
  readonly phase: RunPhase;
  readonly journal: readonly JournalEntry[];
};

export type RunEvent =
  | { readonly type: "pause"; readonly step: string }
  | { readonly type: "resume" }
  | { readonly type: "applied"; readonly call: Call; readonly value: JsonValue }
  | { readonly type: "fail"; readonly error: WorkflowError }
  | { readonly type: "rollback" };

/** A run the reducer accepted, or the rule that refused the event. */
export type RunOutcome =
  | { readonly kind: "reduced"; readonly run: RunState }
  | { readonly kind: "refused"; readonly error: WorkflowError };

export type StartOutcome =
  | { readonly kind: "started"; readonly run: RunState }
  | { readonly kind: "refused"; readonly error: WorkflowError };

export function phaseName(run: RunState): string {
  return run.phase.phase;
}

export function isTerminal(run: RunState): boolean {
  const phase = run.phase.phase;
  return phase === "completed" || phase === "failed" || phase === "rolled-back";
}

/**
 * The step a caller may run or pause on next, or undefined once the plan is
 * done.
 */
export function expectedStep(run: RunState): string | undefined {
  switch (run.phase.phase) {
    case "running":
      return run.phase.next.name;
    case "paused":
      return run.phase.next.name;
    case "completed":
      return undefined;
    case "failed":
      return undefined;
    case "rolled-back":
      return undefined;
  }
}

export function startRun(plan: Plan): StartOutcome {
  const next = stepAt(plan, 0);
  return next === undefined
    ? { kind: "refused", error: { kind: "empty-plan" } }
    : {
        kind: "started",
        run: { plan, phase: { phase: "running", next }, journal: [] },
      };
}

/**
 * The reducer is the only place a phase changes. Every transition is total on
 * the current phase, so an illegal event is refused and leaves the run
 * untouched instead of throwing.
 */
export function reduce(run: RunState, event: RunEvent): RunOutcome {
  if (event.type === "rollback") {
    return {
      kind: "reduced",
      run: {
        plan: run.plan,
        phase: { phase: "rolled-back" },
        journal: run.journal,
      },
    };
  }
  if (isTerminal(run)) {
    return {
      kind: "refused",
      error: { kind: "not-running", phase: run.phase.phase },
    };
  }
  switch (event.type) {
    case "pause":
      return pause(run, event.step);
    case "resume":
      return resume(run);
    case "applied":
      return apply(run, event.call, event.value);
    case "fail":
      return {
        kind: "reduced",
        run: {
          plan: run.plan,
          phase: { phase: "failed", error: event.error },
          journal: run.journal,
        },
      };
  }
}

function pause(run: RunState, step: string): RunOutcome {
  if (run.phase.phase !== "running") {
    return {
      kind: "refused",
      error: { kind: "not-running", phase: run.phase.phase },
    };
  }
  const next = run.phase.next;
  if (next.name !== step) {
    return {
      kind: "refused",
      error: { kind: "out-of-order", expected: next.name, got: step },
    };
  }
  return {
    kind: "reduced",
    run: {
      plan: run.plan,
      phase: { phase: "paused", step, next },
      journal: run.journal,
    },
  };
}

function resume(run: RunState): RunOutcome {
  return run.phase.phase === "paused"
    ? {
        kind: "reduced",
        run: {
          plan: run.plan,
          phase: { phase: "running", next: run.phase.next },
          journal: run.journal,
        },
      }
    : { kind: "refused", error: { kind: "not-paused" } };
}

function apply(run: RunState, call: Call, value: JsonValue): RunOutcome {
  if (run.phase.phase !== "running") {
    return {
      kind: "refused",
      error: { kind: "not-running", phase: run.phase.phase },
    };
  }
  const expected = run.phase.next;
  if (expected.name !== call.step) {
    return {
      kind: "refused",
      error: { kind: "out-of-order", expected: expected.name, got: call.step },
    };
  }
  const entry: JournalEntry = {
    seq: run.journal.length,
    step: call.step,
    input: call.input,
    outcome: { status: "applied", value },
  };
  const journal = [...run.journal, entry];
  const next = stepAt(run.plan, journal.length);
  const phase: RunPhase =
    next === undefined
      ? { phase: "completed", value }
      : { phase: "running", next };
  return { kind: "reduced", run: { plan: run.plan, phase, journal } };
}
