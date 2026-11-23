export type WorkflowError =
  | { readonly kind: "empty-plan" }
  | { readonly kind: "duplicate-step"; readonly step: string }
  | {
      readonly kind: "out-of-order";
      readonly expected: string;
      readonly got: string;
    }
  | {
      readonly kind: "replay-divergence";
      readonly seq: number;
      readonly expected: string;
      readonly got: string;
    }
  | { readonly kind: "not-running"; readonly phase: string }
  | { readonly kind: "not-paused" }
  | {
      readonly kind: "call-failed";
      readonly step: string;
      readonly reason: string;
    };
