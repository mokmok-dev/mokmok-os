import { EventEmitter } from "node:events";
import type { PeerSocket } from "../../src/server.ts";

export interface FakeSocket {
  readonly socket: PeerSocket;
  readonly sent: ReadonlyArray<string>;
  readonly closes: ReadonlyArray<number>;
  readonly emitEvent: (event: string, ...args: ReadonlyArray<unknown>) => void;
  readonly flush: () => void;
  readonly pending: () => number;
}

export const fakeSocket = (): FakeSocket => {
  const emitter = new EventEmitter();
  const sent: Array<string> = [];
  const closes: Array<number> = [];
  const callbacks: Array<(error?: Error) => void> = [];

  const socket: PeerSocket = {
    on: (event, listener) => {
      emitter.on(event, listener);
    },
    send: (text, callback) => {
      sent.push(text);
      callbacks.push(callback);
    },
    close: (code) => {
      closes.push(code);
      emitter.emit("close");
    },
  };

  return {
    socket,
    sent,
    closes,
    emitEvent: (event, ...args) => {
      emitter.emit(event, ...args);
    },
    flush: () => {
      while (callbacks.length > 0) {
        callbacks.shift()?.();
      }
    },
    pending: () => callbacks.length,
  };
};
