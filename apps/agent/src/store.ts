import {
  type Call,
  type JsonValue,
  parsePlan,
  type PlanInput,
  reduce,
  type RunEvent,
  type RunOutcome,
  type RunState,
  type StartOutcome,
  startRun,
} from "@mokmok-os/workflow-core";

import { unreachable } from "./boundary.ts";

const RUN_KEY = "run";

/**
 * The one run this object owns, persisted in its Durable Object storage.
 *
 * Each method sends one reducer event and returns the workflow-core union
 * untouched; the boundary decides how a refusal is reported.
 */
export class RunStore {
  constructor(private readonly storage: DurableObjectStorage) {}

  state(): RunState | undefined {
    return this.storage.kv.get<RunState>(RUN_KEY);
  }

  begin(input: PlanInput): StartOutcome {
    const parsed = parsePlan(input);
    switch (parsed.kind) {
      case "parsed": {
        const started = startRun(parsed.plan);
        switch (started.kind) {
          case "started":
            this.save(started.run);
            return started;
          case "refused":
            return started;
          default:
            return unreachable(started);
        }
      }
      case "invalid":
        return { kind: "refused", error: parsed.error };
      default:
        return unreachable(parsed);
    }
  }

  /**
   * Applies a call. A pause blocks no outcome, so a paused run lifts it first
   * and the same call lands, which is how a resumed run continues.
   */
  applyCall(call: Call, value: JsonValue): RunOutcome {
    const run = this.state();
    if (run !== undefined && run.phase.phase === "paused") {
      const resumed = this.dispatch({ type: "resume" });
      switch (resumed.kind) {
        case "reduced":
          break;
        case "refused":
          return resumed;
        default:
          return unreachable(resumed);
      }
    }
    return this.dispatch({ type: "applied", call, value });
  }

  pauseStep(step: string): RunOutcome {
    return this.dispatch({ type: "pause", step });
  }

  private dispatch(event: RunEvent): RunOutcome {
    const run = this.state();
    if (run === undefined) {
      return {
        kind: "refused",
        error: { kind: "not-running", phase: "unstarted" },
      };
    }
    const outcome = reduce(run, event);
    switch (outcome.kind) {
      case "reduced":
        this.save(outcome.run);
        return outcome;
      case "refused":
        return outcome;
      default:
        return unreachable(outcome);
    }
  }

  private save(run: RunState): void {
    this.storage.kv.put(RUN_KEY, run);
  }
}
