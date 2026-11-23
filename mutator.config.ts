import { defineConfig } from "@mizchi/mutator";

export default defineConfig({
  runner: "bun",
  include: ["packages/workflow-core/src/**/*.ts"],
  bunConfig: "bunfig.toml",
  reporters: ["text", "html", "json"],
  thresholds: { high: 100, low: 100, break: 100 },
  failOnSurvived: true,
  critical: ["packages/workflow-core/src/**"],
});
