import { afterAll, beforeAll, describe, expect, test } from "vitest";
import type { WebSocket } from "ws";

import {
  DAEMON_AVAILABLE,
  connectBus,
  cursorAfter,
  openBus,
  publishBus,
  sendBus,
  startBus,
  stopBus,
  type BusHarness,
} from "./harness.ts";

describe.skipIf(!DAEMON_AVAILABLE)("the protocol package against the real bus", () => {
  let harness: BusHarness;
  let publisher: WebSocket;

  beforeAll(async () => {
    harness = await startBus();
    publisher = await connectBus(harness.socket);
  });

  afterAll(async () => {
    publisher?.terminate();
    await stopBus(harness);
  });

  test("a publisher opens, appends, and reads a typed receipt", async () => {
    const opened = await openBus(publisher, null);
    expect(opened.op).toBe("opened");

    const receipt = await publishBus(publisher, { turn: 1, source: "web-e2e" });
    expect(receipt.op).toBe("published");
    if (receipt.op !== "published") {
      return;
    }

    expect(receipt.seq).toBe(0);
    expect(receipt.id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
    expect(receipt.at.secs_since_epoch).toBeGreaterThan(0);
  });

  test("a subscriber reads the committed event and acknowledges its position", async () => {
    const subscriber = await connectBus(harness.socket);
    try {
      const opened = await openBus(subscriber, crypto.randomUUID());
      expect(opened.op).toBe("opened");
      if (opened.op !== "opened") {
        return;
      }

      const batch = await sendBus(subscriber, { op: "pull", max: 10 });
      expect(batch.op).toBe("batch");
      if (batch.op !== "batch") {
        return;
      }

      const event = batch.events.at(-1);
      expect(event?.topic).toBe("global");
      expect(event?.payload).toEqual({ turn: 1, source: "web-e2e" });

      const through = cursorAfter(batch.events, batch.window);
      const acked = await sendBus(subscriber, { op: "ack", through });
      expect(acked).toEqual({ op: "acked", through });
    } finally {
      subscriber.terminate();
    }
  });

  test("a resubscribed subscriber resumes from the acknowledged checkpoint", async () => {
    const subscriber = await connectBus(harness.socket);
    const subscriberId = crypto.randomUUID();

    try {
      await openBus(subscriber, subscriberId);
      const first = await sendBus(subscriber, { op: "pull", max: 10 });
      if (first.op !== "batch") {
        throw new Error(`expected a batch, received ${first.op}`);
      }
      await sendBus(subscriber, { op: "ack", through: cursorAfter(first.events, first.window) });
    } finally {
      subscriber.terminate();
    }

    const receipt = await publishBus(publisher, { turn: 2, source: "web-e2e" });
    expect(receipt.op).toBe("published");

    const resumed = await connectBus(harness.socket);
    try {
      const opened = await openBus(resumed, subscriberId);
      expect(opened.op).toBe("opened");
      if (opened.op !== "opened") {
        return;
      }

      const batch = await sendBus(resumed, { op: "pull", max: 10 });
      expect(batch.op).toBe("batch");
      if (batch.op !== "batch") {
        return;
      }

      expect(batch.events).toHaveLength(1);
      expect(batch.events[0]?.payload).toEqual({ turn: 2, source: "web-e2e" });
      await sendBus(resumed, { op: "ack", through: cursorAfter(batch.events, batch.window) });
    } finally {
      resumed.terminate();
    }
  });
});
