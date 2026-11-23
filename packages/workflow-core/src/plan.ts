import type { WorkflowError } from "./error.ts";

export type JsonValue =
  | string
  | number
  | boolean
  | null
  | readonly JsonValue[]
  | { readonly [key: string]: JsonValue };

export type Approval = "none" | "required";

export type StepSpec = { readonly name: string; readonly approval: Approval };

export type Plan = {
  readonly id: string;
  readonly version: number;
  readonly steps: readonly StepSpec[];
};

export type PlanInput = {
  readonly id: string;
  readonly version: number;
  readonly steps: readonly StepSpec[];
};

/** A parsed plan, or the one rule that rejected it. */
export type PlanOutcome =
  | { readonly kind: "parsed"; readonly plan: Plan }
  | { readonly kind: "invalid"; readonly error: WorkflowError };

/** A plan needs at least one step and each step name must be unique. */
export function parsePlan(input: PlanInput): PlanOutcome {
  if (input.steps.length === 0) {
    return { kind: "invalid", error: { kind: "empty-plan" } };
  }
  const seen = new Set<string>();
  for (const spec of input.steps) {
    if (seen.has(spec.name)) {
      return {
        kind: "invalid",
        error: { kind: "duplicate-step", step: spec.name },
      };
    }
    seen.add(spec.name);
  }
  return {
    kind: "parsed",
    plan: { id: input.id, version: input.version, steps: input.steps },
  };
}

/** The step at an index, or undefined when the index is past the end. */
export function stepAt(plan: Plan, index: number): StepSpec | undefined {
  return plan.steps[index];
}
