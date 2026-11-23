import {
  type CodemodeRuntimeHandle,
  createCodemodeRuntime,
  DynamicWorkerExecutor,
} from "@cloudflare/codemode";
import type { PlanInput } from "@mokmok-os/workflow-core";
import { Lifecycle } from "agents/lifecycle";
import { DurableObject } from "cloudflare:workers";

import {
  pendingResponse,
  rejectedResponse,
  startWire,
  stateResponse,
  toolResponse,
  wireResponse,
} from "./boundary.ts";
import { WorkflowConnector } from "./connectors/workflow.ts";
import { RunStore } from "./store.ts";

/**
 * The one workflow run this object owns, plus the Code Mode runtime whose
 * connector mutates it.
 *
 * Methods return `Response` rather than a run: `RunState` carries `JsonValue`,
 * whose recursion overflows the RPC provider's type mapping. Workflow unions
 * are narrowed here and only their failure arm leaves as `{ ok: false, error
 * }`.
 */
export class WorkflowRunner extends DurableObject<Env> {
  private readonly store: RunStore;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.store = new RunStore(ctx.storage);
    // Installs the platform fetch, alarm, and WebSocket handlers.
    Lifecycle.install(this);
  }

  state(): Response {
    return stateResponse(this.store.state());
  }

  begin(input: PlanInput): Response {
    return wireResponse(startWire(this.store.begin(input)), 409);
  }

  async codemode(code: string): Promise<Response> {
    return toolResponse(await this.runtime().execute({ code }));
  }

  async pending(): Promise<Response> {
    return pendingResponse(await this.runtime().pending());
  }

  async approve(executionId: string): Promise<Response> {
    return toolResponse(await this.runtime().approve({ executionId }));
  }

  async reject(executionId: string, seq: number): Promise<Response> {
    return rejectedResponse(await this.runtime().reject({ seq, executionId }));
  }

  private runtime(): CodemodeRuntimeHandle {
    return createCodemodeRuntime({
      ctx: this.ctx,
      executor: new DynamicWorkerExecutor({ loader: this.env.LOADER }),
      connectors: [new WorkflowConnector(this.ctx, this.env, this.store)],
    });
  }
}
