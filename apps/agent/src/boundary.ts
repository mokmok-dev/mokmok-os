import type { PendingAction, ProxyToolOutput } from "@cloudflare/codemode";
import type {
  RunOutcome,
  RunState,
  StartOutcome,
  WorkflowError,
} from "@mokmok-os/workflow-core";

/**
 * The Worker's boundary layer. Workflow-core unions are narrowed with
 * exhaustive `switch`es here, and every response shape the routes and Code Mode
 * tools return is built here too.
 */

/** The HTTP shape: a run, or the one rule that refused it. */
export type WireOutcome =
  | { readonly ok: true; readonly run: RunState }
  | { readonly ok: false; readonly error: WorkflowError };

/**
 * Compile-time exhaustiveness. A `switch` that falls through to this has a
 * `kind`, `status`, or `ok` unhandled, and the `never` parameter stops the
 * build.
 */
export function unreachable(value: never): never {
  return value;
}

/** Maps `startRun`'s union where it leaves the object. */
export function startWire(outcome: StartOutcome): WireOutcome {
  switch (outcome.kind) {
    case "started":
      return { ok: true, run: outcome.run };
    case "refused":
      return { ok: false, error: outcome.error };
    default:
      return unreachable(outcome);
  }
}

/**
 * Maps `reduce`'s union where it leaves the object, for the sandbox and the
 * routes.
 */
export function runWire(outcome: RunOutcome): WireOutcome {
  switch (outcome.kind) {
    case "reduced":
      return { ok: true, run: outcome.run };
    case "refused":
      return { ok: false, error: outcome.error };
    default:
      return unreachable(outcome);
  }
}

export function stateResponse(run: RunState | undefined): Response {
  return Response.json({ ok: true, run: run ?? null });
}

export function wireResponse(outcome: WireOutcome, status: number): Response {
  switch (outcome.ok) {
    case true:
      return Response.json({ ok: true, run: outcome.run });
    case false:
      return Response.json({ ok: false, error: outcome.error }, { status });
    default:
      return unreachable(outcome);
  }
}

export function toolResponse(output: ProxyToolOutput): Response {
  switch (output.status) {
    case "completed":
    case "paused":
      return Response.json({ ok: true, ...output });
    case "error":
      return Response.json(
        { ok: false, error: output.error, executionId: output.executionId },
        { status: 400 },
      );
    default:
      return unreachable(output);
  }
}

export function pendingResponse(pending: readonly PendingAction[]): Response {
  return Response.json({ ok: true, pending });
}

export function rejectedResponse(rejected: boolean): Response {
  return Response.json({ ok: true, rejected });
}

export function invalidBody(message: string): Response {
  return Response.json(
    { ok: false, error: { kind: "bad-request", message } },
    { status: 400 },
  );
}

export function notFound(route: string): Response {
  return Response.json(
    { ok: false, error: { kind: "not-found", route } },
    { status: 404 },
  );
}
