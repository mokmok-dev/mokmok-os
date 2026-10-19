// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Event } from "@mokmok/web-protocol";

import "../test/polyfills.ts";
import { EventTable } from "../components/event-table.tsx";

afterEach(cleanup);

const NOW_MS = 1_700_000_060_000;

const event = (seq: number, payload: unknown, idSuffix: string): Event => ({
  id: `11111111-1111-4111-8111-${idSuffix.padStart(12, "0")}`,
  topic: "global",
  seq,
  at: { secs_since_epoch: 1_700_000_000, nanos_since_epoch: 0 },
  payload: payload as Event["payload"],
});

const events: readonly Event[] = [
  event(1, { kind: "alpha", note: "first" }, "000000000001"),
  event(2, { kind: "beta", note: "second" }, "000000000002"),
  event(3, { kind: "gamma", note: "third" }, "000000000003"),
];

const renderTable = (
  rows: readonly Event[],
  onSelect: (selected: Event) => void = () => undefined,
) =>
  render(
    <EventTable
      events={rows}
      nowMs={NOW_MS}
      following
      onFollowingChange={() => undefined}
      onClear={() => undefined}
      onSelect={onSelect}
      emptyState={<p>No events yet</p>}
    />,
  );

const bodyRows = (): HTMLElement[] => screen.getAllByRole("row").slice(1);

describe("EventTable", () => {
  it("renders newest first by default", () => {
    renderTable(events);

    const rows = bodyRows();
    expect(rows.map((row) => row.getAttribute("aria-label"))).toEqual([
      "Open event 3",
      "Open event 2",
      "Open event 1",
    ]);
    expect(screen.getByText("Seq")).toBeTruthy();
    expect(screen.getByText("Payload preview")).toBeTruthy();
  });

  it("filters rows from the filter input", () => {
    renderTable(events);

    fireEvent.change(screen.getByLabelText("Filter events"), { target: { value: "beta" } });

    const rows = bodyRows();
    expect(rows).toHaveLength(1);
    expect(rows[0]?.getAttribute("aria-label")).toBe("Open event 2");
  });

  it("shows a filter-specific empty state and can clear it", () => {
    renderTable(events);

    fireEvent.change(screen.getByLabelText("Filter events"), { target: { value: "zzz" } });

    expect(screen.getByText(/No events match/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Clear filter" }));

    expect(bodyRows()).toHaveLength(3);
  });

  it("opens a row on click and on Enter", () => {
    const onSelect = vi.fn();
    renderTable(events, onSelect);

    const first = bodyRows()[0];
    expect(first).toBeDefined();
    if (first === undefined) return;

    fireEvent.click(first);
    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ seq: 3 }));

    fireEvent.keyDown(first, { key: "Enter" });
    expect(onSelect).toHaveBeenCalledTimes(2);
  });

  it("sorts by sequence when the header is activated", () => {
    renderTable(events);

    fireEvent.click(screen.getByRole("button", { name: /Seq/ }));

    expect(bodyRows().map((row) => row.getAttribute("aria-label"))).toEqual([
      "Open event 1",
      "Open event 2",
      "Open event 3",
    ]);
  });

  it("renders the supplied empty state when there are no events", () => {
    renderTable([]);

    expect(screen.getByText("No events yet")).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
  });
});
