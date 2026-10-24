import { Effect } from "effect";
import { makeEventBus } from "../../src/index.ts";

const socketPath = process.argv[2];
const logPath = process.argv[3];

if (socketPath === undefined || logPath === undefined) {
  throw new Error("usage: server.ts <socket> <log>");
}

const program = Effect.gen(function* () {
  yield* makeEventBus({ socketPath, logPath, maxLag: 64 });
  yield* Effect.never;
});

Effect.runPromise(Effect.scoped(program)).catch((error) => {
  console.error(error);
  process.exit(1);
});
