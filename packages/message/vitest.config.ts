import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Stryker 10's vitest runner filters suite-qualified ids while Vitest 5
    // matches the leaf name only, so tests stay flat or every mutant survives.
    include: ["test/**/*.test.ts"],
  },
});
