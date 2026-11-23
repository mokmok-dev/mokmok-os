import type { WorkflowError } from "./error.ts";
import type { JsonValue } from "./plan.ts";

export type EntryOutcome =
  | { readonly status: "applied"; readonly value: JsonValue }
  | { readonly status: "failed"; readonly error: WorkflowError };

export type JournalEntry = {
  readonly seq: number;
  readonly step: string;
  readonly input: string;
  readonly outcome: EntryOutcome;
};

export type Call = { readonly step: string; readonly input: string };

export type ReplayDecision =
  | { readonly kind: "replay"; readonly value: JsonValue }
  | { readonly kind: "execute" }
  | { readonly kind: "rejected"; readonly error: WorkflowError };

/**
 * The determinism rule. `issued` is how many calls this pass has already made,
 * so a resumed pass lines its calls up against the recorded prefix by position.
 * A recorded step or input that differs ends the run rather than silently
 * executing something else.
 */
export function decideReplay(
  journal: readonly JournalEntry[],
  issued: number,
  call: Call,
): ReplayDecision {
  const recorded = journal[issued];
  if (recorded === undefined) {
    return { kind: "execute" };
  }
  if (recorded.step !== call.step) {
    return {
      kind: "rejected",
      error: {
        kind: "replay-divergence",
        seq: issued,
        expected: recorded.step,
        got: call.step,
      },
    };
  }
  if (recorded.input !== call.input) {
    return {
      kind: "rejected",
      error: {
        kind: "replay-divergence",
        seq: issued,
        expected: recorded.input,
        got: call.input,
      },
    };
  }
  if (recorded.outcome.status === "failed") {
    return { kind: "rejected", error: recorded.outcome.error };
  }
  return { kind: "replay", value: recorded.outcome.value };
}
