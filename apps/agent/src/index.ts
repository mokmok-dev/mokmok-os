import type { PlanInput } from "@mokmok-os/workflow-core";
import { z } from "zod";

import { invalidBody, notFound, unreachable } from "./boundary.ts";
import { WorkflowRunner } from "./runner.ts";

export { CodemodeRuntime } from "@cloudflare/codemode";
export { WorkflowRunner };

const RUNNER_NAME = "default";

const beginBody: z.ZodType<PlanInput> = z.object({
  id: z.string(),
  version: z.number(),
  steps: z.array(
    z.object({ name: z.string(), approval: z.enum(["none", "required"]) }),
  ),
});

const codemodeBody = z.object({ code: z.string() });
const approveBody = z.object({ executionId: z.string() });
const rejectBody = z.object({ seq: z.number(), executionId: z.string() });

type Parsed<S extends z.ZodType> =
  | { readonly ok: true; readonly value: z.output<S> }
  | { readonly ok: false; readonly error: string };

/**
 * The HTTP boundary: parse the body, call one runner method, relay its
 * response.
 */
export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const runner = env.RUNNER.getByName(RUNNER_NAME);
    const { pathname } = new URL(request.url);
    const key = `${request.method} ${pathname}`;
    switch (key) {
      case "GET /state":
        return guard(() => runner.state());
      case "POST /begin": {
        const body = await parseBody(request, beginBody);
        switch (body.ok) {
          case false:
            return invalidBody(body.error);
          case true:
            return guard(() => runner.begin(body.value));
          default:
            return unreachable(body);
        }
      }
      case "POST /codemode": {
        const body = await parseBody(request, codemodeBody);
        switch (body.ok) {
          case false:
            return invalidBody(body.error);
          case true:
            return guard(() => runner.codemode(body.value.code));
          default:
            return unreachable(body);
        }
      }
      case "GET /pending":
        return guard(() => runner.pending());
      case "POST /approve": {
        const body = await parseBody(request, approveBody);
        switch (body.ok) {
          case false:
            return invalidBody(body.error);
          case true:
            return guard(() => runner.approve(body.value.executionId));
          default:
            return unreachable(body);
        }
      }
      case "POST /reject": {
        const body = await parseBody(request, rejectBody);
        switch (body.ok) {
          case false:
            return invalidBody(body.error);
          case true:
            return guard(() =>
              runner.reject(body.value.executionId, body.value.seq),
            );
          default:
            return unreachable(body);
        }
      }
      default:
        return notFound(key);
    }
  },
} satisfies ExportedHandler<Env>;

async function parseBody<S extends z.ZodType>(
  request: Request,
  schema: S,
): Promise<Parsed<S>> {
  let json: unknown;
  try {
    json = await request.json();
  } catch {
    return { ok: false, error: "request body must be JSON" };
  }
  const parsed = schema.safeParse(json);
  return parsed.success
    ? { ok: true, value: parsed.data }
    : { ok: false, error: z.prettifyError(parsed.error) };
}

/** A failed RPC or runtime call becomes a 502 instead of an escaping exception. */
async function guard(call: () => Promise<Response>): Promise<Response> {
  try {
    return await call();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return Response.json(
      { ok: false, error: { kind: "codemode-failed", message } },
      { status: 502 },
    );
  }
}
