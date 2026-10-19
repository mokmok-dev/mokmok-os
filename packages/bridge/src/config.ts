import { join, resolve } from "node:path";

import type { BridgeConfig } from "./types.ts";

const UUID_SOURCE = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
const TOPIC_PATTERN = new RegExp(`^(global|(agent|session|execution):${UUID_SOURCE})$`);

const DEFAULT_TOPIC = "global";
const DEFAULT_HOST = "127.0.0.1";
const DEFAULT_PORT = 8787;
const DEFAULT_DATA_DIR = ".mokmok-web";
const DEFAULT_STATIC_DIR = "packages/console/dist";
const DEFAULT_SOCKET = "/tmp/mokmok/bus.sock";

export const resolveBusSocketPath = (
  env: Readonly<Record<string, string | undefined>>,
): string => {
  const explicit = env.MOKMOK_WEB_SOCKET?.trim();
  if (explicit !== undefined && explicit !== "") return explicit;

  const runtimeDir = env.XDG_RUNTIME_DIR?.trim();
  if (runtimeDir !== undefined && runtimeDir !== "") {
    return join(runtimeDir, "mokmok", "bus.sock");
  }

  return DEFAULT_SOCKET;
};

const readText = (
  env: Readonly<Record<string, string | undefined>>,
  key: string,
): string | undefined => {
  const value = env[key]?.trim();

  return value === undefined || value === "" ? undefined : value;
};

export const parseBridgeConfig = (
  env: Readonly<Record<string, string | undefined>>,
): BridgeConfig => {
  const topic = readText(env, "MOKMOK_WEB_TOPIC") ?? DEFAULT_TOPIC;
  if (!TOPIC_PATTERN.test(topic)) {
    throw new Error(
      `MOKMOK_WEB_TOPIC must be "global" or "<agent|session|execution>:<uuid>", got ${JSON.stringify(topic)}`,
    );
  }

  const portText = env.MOKMOK_WEB_PORT?.trim() ?? "";
  const port = portText === "" ? DEFAULT_PORT : Number(portText);
  if (!Number.isInteger(port) || port < 0 || port > 65_535) {
    throw new Error(
      `MOKMOK_WEB_PORT must be an integer from 0 through 65535, got ${JSON.stringify(portText)}`,
    );
  }

  const dataDir = readText(env, "MOKMOK_WEB_DATA_DIR") ?? DEFAULT_DATA_DIR;
  const staticDir = readText(env, "MOKMOK_WEB_STATIC_DIR") ?? DEFAULT_STATIC_DIR;

  return {
    socketPath: resolveBusSocketPath(env),
    topic,
    subscriberFile: resolve(dataDir, "subscriber-id"),
    host: readText(env, "MOKMOK_WEB_HOST") ?? DEFAULT_HOST,
    port,
    staticDir: resolve(staticDir),
  };
};
