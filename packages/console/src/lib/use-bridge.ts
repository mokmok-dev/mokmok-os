import { Effect, Fiber, Stream } from "effect";
import type { Json } from "effect/Schema";
import { useEffect, useMemo, useRef, useState } from "react";

import type { AckFrame, BridgeServerFrame } from "@mokmok/web-protocol";

import { makeBridgeClient, type BridgeClient, type PublishOutcome } from "./bridge-client.ts";

export interface BridgeHandle {
  readonly publish: (requestId: string, payload: Json) => Promise<PublishOutcome>;
  readonly ack: (frame: AckFrame) => Promise<boolean>;
}

/**
 * Owns the single browser WebSocket for the app. The socket stays open for the lifetime of the
 * provider; unmounting interrupts the connection fiber, whose finalizer closes the socket.
 */
export const useBridge = (onFrame: (frame: BridgeServerFrame) => void, url = "/api/stream"): BridgeHandle => {
  const [client] = useState<BridgeClient>(() => Effect.runSync(makeBridgeClient(url)));
  const frameSink = useRef(onFrame);
  frameSink.current = onFrame;

  useEffect(() => {
    const subscription = Effect.runFork(
      Stream.runForEach(client.frames, (frame) => Effect.sync(() => frameSink.current(frame))),
    );
    const connection = Effect.runFork(client.connect);

    return () => {
      Effect.runFork(Fiber.interrupt(connection));
      Effect.runFork(Fiber.interrupt(subscription));
    };
  }, [client]);

  return useMemo(
    () => ({
      publish: (requestId, payload) => Effect.runPromise(client.publish(requestId, payload)),
      ack: (frame) => Effect.runPromise(client.ack(frame)),
    }),
    [client],
  );
};
