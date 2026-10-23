import { Cause, Deferred, Effect, Queue, Stream } from "effect";
import type { Peer } from "../../src/server.ts";
import type { ServerFrame } from "../../src/wire.ts";

export interface FakePeer {
  readonly peer: Peer;
  readonly frames: ReadonlyArray<ServerFrame>;
  readonly texts: ReadonlyArray<string>;
  readonly closes: ReadonlyArray<number>;
  readonly push: (text: string) => Effect.Effect<void>;
  readonly end: Effect.Effect<void>;
}

export interface FakePeerOptions {
  readonly sendGate?: Deferred.Deferred<void>;
  readonly sendEntered?: Deferred.Deferred<void>;
}

export const fakePeer = (options: FakePeerOptions = {}): FakePeer => {
  const inbound = Queue.bounded<string, Cause.Done>(64).pipe(Effect.runSync);
  const frames: Array<ServerFrame> = [];
  const texts: Array<string> = [];
  const closes: Array<number> = [];
  let closed = false;

  const record = (text: string): Effect.Effect<void> =>
    Effect.sync(() => {
      texts.push(text);
      frames.push(JSON.parse(text) as ServerFrame);
    });

  const peer: Peer = {
    messages: Stream.fromQueue(inbound),
    send: (text) =>
      Effect.andThen(
        options.sendEntered === undefined
          ? Effect.void
          : Deferred.succeed(options.sendEntered, void 0),
        Effect.andThen(
          options.sendGate === undefined ? Effect.void : Deferred.await(options.sendGate),
          record(text),
        ),
      ),
    close: (code) =>
      Effect.sync(() => {
        if (closed) return;
        closed = true;
        closes.push(code);
        Queue.endUnsafe(inbound);
      }),
  };

  return {
    peer,
    frames,
    texts,
    closes,
    push: (text) => Queue.offer(inbound, text).pipe(Effect.asVoid),
    end: Effect.sync(() => {
      Queue.endUnsafe(inbound);
    }),
  };
};
