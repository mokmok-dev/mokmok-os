import { spawn } from "node:child_process";

const root = new URL("..", import.meta.url).pathname;
const port = process.env.MOKMOK_WEB_PORT ?? "8787";
const children = [
  spawn("node", ["packages/bridge/src/cli.ts"], {
    cwd: root,
    stdio: "inherit",
    env: {
      ...process.env,
      MOKMOK_WEB_PORT: port,
      MOKMOK_WEB_STATIC_DIR: "packages/console/dist",
    },
  }),
  spawn("pnpm", ["--filter", "@mokmok/web-console", "dev"], {
    cwd: root,
    stdio: "inherit",
    env: { ...process.env, MOKMOK_WEB_PORT: port },
  }),
];

const stop = () => {
  for (const child of children) {
    child.kill("SIGTERM");
  }
};

process.on("SIGINT", stop);
process.on("SIGTERM", stop);
for (const child of children) {
  child.on("exit", (code) => {
    if (code !== 0 && code !== null) {
      stop();
      process.exitCode = code;
    }
  });
}
