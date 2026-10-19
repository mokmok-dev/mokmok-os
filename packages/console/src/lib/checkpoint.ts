import { hydrate, persistable, type ConsumerState } from "@mokmok/web-consumer";

export const DEFAULT_TOPIC = "global";

export const checkpointKey = (topic: string): string => `mokmok.web.checkpoint.${topic}`;

export type CheckpointWrite = { readonly ok: true } | { readonly ok: false; readonly message: string };

export const readCheckpoint = (topic: string): ConsumerState => {
  try {
    return hydrate(window.localStorage.getItem(checkpointKey(topic)), topic);
  } catch {
    return hydrate(null, topic);
  }
};

export const writeCheckpoint = (state: ConsumerState): CheckpointWrite => {
  try {
    window.localStorage.setItem(checkpointKey(state.topic), persistable(state));
    return { ok: true };
  } catch (error) {
    return {
      ok: false,
      message: error instanceof Error ? error.message : "localStorage refused the write",
    };
  }
};
