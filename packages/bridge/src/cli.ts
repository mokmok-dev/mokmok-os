#!/usr/bin/env node
import { NodeRuntime } from "@effect/platform-node";
import { Effect } from "effect";

import { parseBridgeConfig } from "./config.ts";
import { startBridge } from "./server.ts";

const program = Effect.scoped(
  Effect.gen(function* () {
    const config = yield* Effect.sync(() => parseBridgeConfig(process.env));
    const handle = yield* Effect.tryPromise({
      try: () => startBridge(config),
      catch: (cause) =>
        new Error(
          cause instanceof Error
            ? cause.message
            : `the bridge could not start: ${String(cause)}`,
        ),
    });

    yield* Effect.addFinalizer(() => Effect.promise(() => handle.close()));
    yield* Effect.logInfo(
      `mokmok web bridge listening on http://${config.host}:${handle.port} (topic ${config.topic}, subscriber ${handle.subscriberId}, bus ${config.socketPath})`,
    );
    yield* Effect.never;
  }),
);

NodeRuntime.runMain(program);
