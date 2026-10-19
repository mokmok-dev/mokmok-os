// @vitest-environment jsdom
import { Toasty } from "@cloudflare/kumo/components/toast";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { installFakeWebSocket, sockets } from "./test/fake-socket.ts";
import "./test/polyfills.ts";
import { ConsoleProvider } from "./lib/console-context.tsx";
import { router } from "./router.tsx";

const TOPIC = "global";
const EPOCH = "11111111-1111-4111-8111-111111111111";
const SUBSCRIBER = "22222222-2222-4222-8222-222222222222";
const EVENT_ID = "33333333-3333-4333-8333-333333333333";
const PUBLISHED_ID = "55555555-5555-4555-8555-555555555555";

const realWebSocket = installFakeWebSocket();

const healthDocument = {
  status: "live",
  topic: TOPIC,
  subscriber: SUBSCRIBER,
  socket: "/run/mokmok/bus.sock",
  events_delivered: 1,
  last_seq: 1,
  pending_batch: null,
  connected_at_ms: 1_700_000_000_000,
};

const renderApp = () => {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <Toasty>
        <ConsoleProvider>
          <RouterProvider router={router} />
        </ConsoleProvider>
      </Toasty>
    </QueryClientProvider>,
  );
};

const openSocket = async () => {
  await waitFor(() => expect(sockets).toHaveLength(1));
  const socket = sockets[0];
  if (socket === undefined) throw new Error("no fake socket was created");
  await waitFor(() => expect(socket.readyState).toBe(1));
  return socket;
};

describe("the console app", () => {
  beforeEach(() => {
    installFakeWebSocket();
    window.localStorage.clear();
    window.history.replaceState(null, "", "/");
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify(healthDocument), { status: 200 })),
    );
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  afterAll(() => {
    globalThis.WebSocket = realWebSocket;
  });

  it("renders the stream, opens an event, and publishes from the publish route", async () => {
    renderApp();

    expect(await screen.findByRole("heading", { level: 1, name: "Stream" })).toBeTruthy();
    expect(screen.getByText("mokmok")).toBeTruthy();
    expect(await screen.findByText("/run/mokmok/bus.sock")).toBeTruthy();

    const socket = await openSocket();
    socket.receive({ op: "hello", topic: TOPIC, subscriber: SUBSCRIBER });
    socket.receive({ op: "status", connection: "live", detail: null });
    socket.receive({
      op: "batch",
      batch_id: "batch-1",
      window: { epoch: EPOCH, first: 1, next: 2 },
      lapsed: null,
      events: [
        {
          id: EVENT_ID,
          topic: TOPIC,
          seq: 1,
          at: { secs_since_epoch: 1_700_000_000, nanos_since_epoch: 0 },
          payload: { kind: "smoke" },
        },
      ],
    });

    const row = await screen.findByLabelText("Open event 1");
    fireEvent.click(row);
    expect(await screen.findByRole("dialog")).toBeTruthy();
    expect(screen.getByLabelText("Event payload")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Close event details" }));

    fireEvent.click(screen.getByRole("button", { name: "Publish" }));
    expect(await screen.findByRole("heading", { level: 1, name: "Publish" })).toBeTruthy();

    const composer = screen.getByLabelText("Payload (JSON)");
    fireEvent.change(composer, { target: { value: '{"kind":"outbound"}' } });
    fireEvent.click(screen.getByRole("button", { name: /Publish event/ }));

    await waitFor(() =>
      expect(socket.frames().some((frame) => frame.op === "publish")).toBe(true),
    );
    const publishFrame = socket.sent
      .map((text) => JSON.parse(text) as { op: string; request_id?: string })
      .find((frame) => frame.op === "publish");
    expect(publishFrame?.request_id).toBeTruthy();
    const requestId = publishFrame?.request_id;
    if (requestId === undefined) throw new Error("publish frame had no request id");

    socket.receive({
      op: "published",
      request_id: requestId,
      id: PUBLISHED_ID,
      seq: 2,
      at: { secs_since_epoch: 1_700_000_001, nanos_since_epoch: 0 },
    });

    expect(await screen.findAllByText(requestId)).not.toHaveLength(0);
    expect((await screen.findAllByText("Confirmed")).length).toBeGreaterThan(0);
  });
});
