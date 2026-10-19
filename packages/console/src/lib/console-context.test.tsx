// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { BatchFrame, BridgeServerFrame } from "@mokmok/web-protocol";

import { installFakeWebSocket, sockets, type FakeSocket } from "../test/fake-socket.ts";
import "../test/polyfills.ts";
import { ConsoleProvider, useConsole } from "./console-context.tsx";

const TOPIC = "global";
const EPOCH = "11111111-1111-4111-8111-111111111111";
const SUBSCRIBER = "22222222-2222-4222-8222-222222222222";

const realWebSocket = installFakeWebSocket();

const eventFrame = (): BatchFrame => ({
  op: "batch",
  batch_id: "batch-1",
  window: { epoch: EPOCH, first: 1, next: 2 },
  lapsed: null,
  events: [
    {
      id: "33333333-3333-4333-8333-333333333333",
      topic: TOPIC,
      seq: 1,
      at: { secs_since_epoch: 1_700_000_000, nanos_since_epoch: 0 },
      payload: { kind: "probe" },
    },
  ],
});

const Probe = () => {
  const { state, storageError } = useConsole();
  return (
    <div>
      <span data-testid="phase">{state.phase}</span>
      <span data-testid="events">{state.events.length}</span>
      <span data-testid="pending">{state.pending === null ? "none" : state.pending.batchId}</span>
      <span data-testid="storage">{storageError ?? "none"}</span>
    </div>
  );
};

const mount = () =>
  render(
    <ConsoleProvider>
      <Probe />
    </ConsoleProvider>,
  );

const connectedSocket = async (): Promise<FakeSocket> => {
  await waitFor(() => expect(sockets).toHaveLength(1));
  const socket = sockets[0];
  if (socket === undefined) throw new Error("no fake socket was created");
  await waitFor(() => expect(socket.readyState).toBe(1));
  return socket;
};

const receive = async (socket: FakeSocket, frames: readonly BridgeServerFrame[]): Promise<void> => {
  await act(async () => {
    for (const frame of frames) socket.receive(frame);
  });
};

describe("ConsoleProvider checkpoint persistence", () => {
  beforeEach(() => {
    installFakeWebSocket();
    window.localStorage.clear();
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  afterAll(() => {
    globalThis.WebSocket = realWebSocket;
  });

  it("writes the checkpoint before acknowledging a batch", async () => {
    const order: string[] = [];
    const setItem = window.localStorage.setItem.bind(window.localStorage);
    vi.spyOn(window.localStorage, "setItem").mockImplementation((key, value) => {
      order.push("persist");
      setItem(key, value);
    });

    mount();
    const socket = await connectedSocket();
    const send = socket.send.bind(socket);
    vi.spyOn(socket, "send").mockImplementation((text) => {
      order.push("send");
      send(text);
    });

    await receive(socket, [
      { op: "hello", topic: TOPIC, subscriber: SUBSCRIBER },
      { op: "status", connection: "live", detail: null },
      eventFrame(),
    ]);

    await waitFor(() => expect(screen.getByTestId("events").textContent).toBe("1"));

    const stored = window.localStorage.getItem(`mokmok.web.checkpoint.${TOPIC}`);
    expect(stored).not.toBeNull();
    const document = JSON.parse(stored ?? "{}") as { events?: unknown[] };
    expect(document.events).toHaveLength(1);

    const ack = socket.frames().find((frame) => frame.op === "ack");
    expect(ack).toBeDefined();
    expect(ack).toMatchObject({
      op: "ack",
      batch_id: "batch-1",
      through: { epoch: EPOCH, seq: 2 },
    });
    expect(order.indexOf("persist")).toBeGreaterThanOrEqual(0);
    expect(order.indexOf("persist")).toBeLessThan(order.indexOf("send"));

    await receive(socket, [{ op: "acked", through: { epoch: EPOCH, seq: 2 } }]);
    await waitFor(() => expect(screen.getByTestId("pending").textContent).toBe("none"));
  });

  it("hydrates the initial state from the checkpoint", () => {
    window.localStorage.setItem(
      `mokmok.web.checkpoint.${TOPIC}`,
      JSON.stringify({
        version: 1,
        topic: TOPIC,
        subscriber: SUBSCRIBER,
        checkpoint: { epoch: EPOCH, seq: 7 },
        events: [
          {
            id: "44444444-4444-4444-8444-444444444444",
            topic: TOPIC,
            seq: 7,
            at: { secs_since_epoch: 1_700_000_000, nanos_since_epoch: 0 },
            payload: null,
          },
        ],
        evicted: 0,
      }),
    );

    mount();

    expect(screen.getByTestId("events").textContent).toBe("1");
  });

  it("does not acknowledge and reports offline when storage refuses the checkpoint", async () => {
    vi.spyOn(window.localStorage, "setItem").mockImplementation(() => {
      throw new DOMException("quota exceeded", "QuotaExceededError");
    });

    mount();
    const socket = await connectedSocket();

    await receive(socket, [
      { op: "hello", topic: TOPIC, subscriber: SUBSCRIBER },
      { op: "status", connection: "live", detail: null },
      eventFrame(),
    ]);

    await waitFor(() => expect(screen.getByTestId("storage").textContent).not.toBe("none"));
    expect(screen.getByTestId("phase").textContent).toBe("offline");
    expect(screen.getByTestId("pending").textContent).toBe("batch-1");
    expect(socket.frames().some((frame) => frame.op === "ack")).toBe(false);
  });
});
