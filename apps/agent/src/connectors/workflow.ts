import { CodemodeConnector, type ConnectorTools } from "@cloudflare/codemode";
import type { JsonValue } from "@mokmok-os/workflow-core";
import { z } from "zod";

import { runWire } from "../boundary.ts";
import type { RunStore } from "../store.ts";

const applyCallInput = z.object({
  step: z.string(),
  input: z.string(),
  value: z.json().optional(),
});

const stepInput = z.object({ step: z.string() });

/**
 * Bridges the run this object owns into the Code Mode sandbox.
 *
 * The sandbox proxy forwards only the first positional argument, so every tool
 * takes one object. Both writes require approval: the journal changes only
 * after a person has seen the call. Each tool is one reducer event, mapped
 * where it leaves the store.
 */
export class WorkflowConnector extends CodemodeConnector<Env> {
  constructor(
    ctx: DurableObjectState,
    env: Env,
    private readonly store: RunStore,
  ) {
    super(ctx, env);
  }

  name(): string {
    return "workflow";
  }

  protected override instructions(): string {
    return "Reads and advances the one workflow run this object owns. applyCall journals a step result; pauseStep parks the run for a person.";
  }

  protected tools(): ConnectorTools {
    return {
      readState: {
        description: "The current run state, or null before a plan has begun.",
        inputSchema: {
          type: "object",
          properties: {},
          additionalProperties: false,
        },
        execute: () => this.store.state() ?? null,
      },
      applyCall: {
        description:
          "Journal the result of one step. Requires approval; on approval the call is reduced into the journal.",
        inputSchema: {
          type: "object",
          properties: {
            step: {
              type: "string",
              description: "The step name being applied.",
            },
            input: {
              type: "string",
              description: "The input this call was made with.",
            },
            value: {
              description: "The step result to journal. Defaults to null.",
            },
          },
          required: ["step", "input"],
          additionalProperties: false,
        },
        requiresApproval: true,
        execute: (args) => {
          const parsed = applyCallInput.parse(args);
          const value: JsonValue = parsed.value ?? null;
          return runWire(
            this.store.applyCall(
              { step: parsed.step, input: parsed.input },
              value,
            ),
          );
        },
      },
      pauseStep: {
        description:
          "Park the run on the step it is about to run, waiting for a person. The next applyCall continues from there.",
        inputSchema: {
          type: "object",
          properties: { step: { type: "string" } },
          required: ["step"],
          additionalProperties: false,
        },
        requiresApproval: true,
        execute: (args) =>
          runWire(this.store.pauseStep(stepInput.parse(args).step)),
      },
    };
  }
}
