import type { BridgeServerFrame } from "@mokmok/web-protocol";

export const sockets: FakeSocket[] = [];

export class FakeSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  readyState = FakeSocket.CONNECTING;
  readonly sent: string[] = [];
  private readonly listeners = new Map<string, Set<(event: Event) => void>>();

  readonly url: string;

  constructor(url: string) {
    this.url = url;
    sockets.push(this);
    queueMicrotask(() => {
      this.readyState = FakeSocket.OPEN;
      this.dispatch("open", new Event("open"));
    });
  }

  addEventListener(type: string, listener: (event: Event) => void): void {
    const bucket = this.listeners.get(type) ?? new Set();
    bucket.add(listener);
    this.listeners.set(type, bucket);
  }

  removeEventListener(type: string, listener: (event: Event) => void): void {
    this.listeners.get(type)?.delete(listener);
  }

  send(text: string): void {
    this.sent.push(text);
  }

  close(): void {
    this.readyState = FakeSocket.CLOSED;
    this.dispatch("close", new Event("close"));
  }

  receive(frame: BridgeServerFrame): void {
    this.dispatch("message", { data: JSON.stringify(frame) } as MessageEvent);
  }

  frames(): Array<{ op: string }> {
    return this.sent.map((text) => JSON.parse(text) as { op: string });
  }

  private dispatch(type: string, event: Event): void {
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

export const installFakeWebSocket = (): typeof WebSocket => {
  const real = globalThis.WebSocket;
  sockets.length = 0;
  globalThis.WebSocket = FakeSocket as unknown as typeof WebSocket;
  return real;
};
