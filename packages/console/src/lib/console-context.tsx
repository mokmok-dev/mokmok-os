import { batchThrough, reduce, type ConsumerState } from "@mokmok/web-consumer";
import type { Json } from "effect/Schema";
import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";

import type { AckFrame, BatchFrame, BridgeServerFrame } from "@mokmok/web-protocol";

import { DEFAULT_TOPIC, readCheckpoint, writeCheckpoint } from "./checkpoint.ts";
import { frameToMessage } from "./frames.ts";
import { useBridge } from "./use-bridge.ts";
import type { PublishOutcome } from "./bridge-client.ts";
import { useNow } from "./use-now.ts";

const STORAGE_FAILURE_DETAIL =
  "The checkpoint could not be written to localStorage, so the console stopped acknowledging batches.";

interface ConsoleContextValue {
  readonly state: ConsumerState;
  readonly storageError: string | null;
  readonly nowMs: number;
  readonly publish: (requestId: string, payload: Json) => Promise<PublishOutcome>;
  readonly clearEvents: () => void;
  readonly retryPersist: () => void;
}

const ConsoleContext = createContext<ConsoleContextValue | null>(null);

const ackFor = (frame: BatchFrame): AckFrame => ({
  op: "ack",
  batch_id: frame.batch_id,
  through: batchThrough(frame),
});

export const ConsoleProvider = ({ children }: { readonly children: ReactNode }) => {
  const [state, setState] = useState<ConsumerState>(() => readCheckpoint(DEFAULT_TOPIC));
  const [storageError, setStorageError] = useState<string | null>(null);
  const stateRef = useRef(state);
  const pendingAckRef = useRef<AckFrame | null>(null);
  const hydratedTopicRef = useRef<string | null>(null);
  const nowMs = useNow();

  const commit = useCallback((next: ConsumerState) => {
    stateRef.current = next;
    setState(next);
  }, []);

  const frameSinkRef = useRef<(frame: BridgeServerFrame) => void>(() => undefined);
  const bridge = useBridge((frame) => frameSinkRef.current(frame));

  const sendAck = useCallback(
    (frame: BatchFrame) => {
      pendingAckRef.current = null;
      void bridge.ack(ackFor(frame)).catch(() => undefined);
    },
    [bridge],
  );

  const handleFrame = useCallback(
    (frame: BridgeServerFrame) => {
      if (frame.op === "batch") {
        const next = reduce(stateRef.current, { type: "batch", frame });
        commit(next);
        const written = writeCheckpoint(next);
        if (written.ok) {
          setStorageError(null);
          sendAck(frame);
        } else {
          pendingAckRef.current = ackFor(frame);
          setStorageError(written.message);
          commit(
            reduce(next, {
              type: "status",
              connection: "offline",
              detail: STORAGE_FAILURE_DETAIL,
            }),
          );
        }
        return;
      }

      if (frame.op === "hello" && hydratedTopicRef.current === null && stateRef.current.topic !== frame.topic) {
        hydratedTopicRef.current = frame.topic;
        const restored = readCheckpoint(frame.topic);
        commit(reduce(restored, frameToMessage(frame)));
        return;
      }

      if (frame.op === "hello") hydratedTopicRef.current = frame.topic;
      commit(reduce(stateRef.current, frameToMessage(frame)));
    },
    [commit, sendAck],
  );

  frameSinkRef.current = handleFrame;

  const clearEvents = useCallback(() => {
    const next = reduce(stateRef.current, { type: "clear_events" });
    commit(next);
    const written = writeCheckpoint(next);
    setStorageError(written.ok ? null : written.message);
  }, [commit]);

  const retryPersist = useCallback(() => {
    const next = stateRef.current;
    const written = writeCheckpoint(next);
    if (!written.ok) {
      setStorageError(written.message);
      return;
    }
    setStorageError(null);
    commit(reduce(next, { type: "status", connection: "live", detail: null }));
    const pendingAck = pendingAckRef.current;
    if (pendingAck !== null) {
      pendingAckRef.current = null;
      void bridge.ack(pendingAck).catch(() => undefined);
    }
  }, [bridge, commit]);

  const value = useMemo<ConsoleContextValue>(
    () => ({
      state,
      storageError,
      nowMs,
      publish: bridge.publish,
      clearEvents,
      retryPersist,
    }),
    [state, storageError, nowMs, bridge, clearEvents, retryPersist],
  );

  return <ConsoleContext.Provider value={value}>{children}</ConsoleContext.Provider>;
};

export const useConsole = (): ConsoleContextValue => {
  const value = useContext(ConsoleContext);
  if (value === null) throw new Error("useConsole must be used inside ConsoleProvider");
  return value;
};
