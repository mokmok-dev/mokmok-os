import { connect as connectSocket } from "node:net";

import { Cause, Effect, Queue } from "effect";
import { WebSocket, type RawData } from "ws";

import {
  encodeBusRequest,
  parseBusResponse,
  type BusRequest,
  type BusResponse,
} from "@mokmok/web-protocol";

import { BusTransportError } from "./errors.ts";
import { rawDataToText } from "./raw.ts";

const BUS_URL = "ws://localhost/ws";

export interface BusConnection {
  readonly responses: Queue.Dequeue<BusResponse, BusTransportError>;
  send(request: BusRequest): Effect.Effect<void, BusTransportError>;
  close(): Effect.Effect<void>;
}

const openWebSocket = (
  socketPath: string,
): Effect.Effect<WebSocket, BusTransportError> =>
  Effect.callback<WebSocket, BusTransportError>((resume) => {
    let settled = false;
    const socket = new WebSocket(BUS_URL, {
      createConnection: () => connectSocket(socketPath),
    });

    const settle = (effect: Effect.Effect<WebSocket, BusTransportError>) => {
      if (settled) return;
      settled = true;
      socket.off("open", onOpen);
      socket.off("error", onError);
      resume(effect);
    };

    const onOpen = () => settle(Effect.succeed(socket));
    const onError = (error: Error) =>
      settle(
        Effect.fail(
          new BusTransportError({
            detail: `cannot connect to the bus at ${socketPath}: ${error.message}`,
          }),
        ),
      );

    // Keep a permanent error listener so a late socket error never becomes an
    // uncaught EventEmitter error after the acquisition settles.
    socket.on("error", () => {});
    socket.once("open", onOpen);
    socket.once("error", onError);

    return Effect.sync(() => {
      if (settled) return;
      settled = true;
      try {
        socket.terminate();
      } catch {
        // The socket was already gone.
      }
    });
  });

export const connectBus = (
  socketPath: string,
): Effect.Effect<BusConnection, BusTransportError> =>
  Effect.gen(function* () {
    const socket = yield* openWebSocket(socketPath);
    const responses = yield* Queue.unbounded<BusResponse, BusTransportError>();

    let disposed = false;

    const fail = (detail: string) => {
      if (disposed) return;
      disposed = true;
      Queue.failCauseUnsafe(
        responses,
        Cause.fail(new BusTransportError({ detail })),
      );
    };

    socket.on("message", (data: RawData) => {
      if (disposed) return;

      const response = parseBusResponse(rawDataToText(data));
      if (response === null) {
        fail("the bus sent a frame the bridge cannot decode");
        try {
          socket.terminate();
        } catch {
          // The socket was already gone.
        }
        return;
      }

      Queue.offerUnsafe(responses, response);
    });
    socket.on("error", (error: Error) => {
      fail(`bus socket error: ${error.message}`);
    });
    socket.on("close", (code: number, reason: Buffer) => {
      const suffix = reason.length === 0 ? "" : `: ${reason.toString("utf8")}`;
      fail(`bus connection closed (${code}${suffix})`);
    });

    const send = (
      request: BusRequest,
    ): Effect.Effect<void, BusTransportError> =>
      Effect.callback<void, BusTransportError>((resume) => {
        if (disposed || socket.readyState !== WebSocket.OPEN) {
          resume(
            Effect.fail(
              new BusTransportError({
                detail: "the bus connection is not open",
              }),
            ),
          );
          return;
        }

        try {
          socket.send(encodeBusRequest(request), (error?: Error) => {
            if (error == null) {
              resume(Effect.void);
            } else {
              resume(
                Effect.fail(
                  new BusTransportError({
                    detail: `could not write to the bus: ${error.message}`,
                  }),
                ),
              );
            }
          });
        } catch (error) {
          resume(
            Effect.fail(
              new BusTransportError({
                detail: `could not write to the bus: ${String(error)}`,
              }),
            ),
          );
        }
      });

    const close = Effect.sync(() => {
      disposed = true;
      Queue.shutdownUnsafe(responses);
      try {
        socket.terminate();
      } catch {
        // The socket was already gone.
      }
    });

    return { responses, send, close: () => close };
  });
