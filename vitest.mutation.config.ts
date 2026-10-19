import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: [
      "packages/protocol/src/**/*.test.ts",
      "packages/consumer/src/**/*.test.ts",
    ],
  },
});
