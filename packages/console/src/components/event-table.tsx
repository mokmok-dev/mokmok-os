import { Input } from "@cloudflare/kumo/components/input";
import { LayerCard } from "@cloudflare/kumo/components/layer-card";
import { Switch } from "@cloudflare/kumo/components/switch";
import { Table } from "@cloudflare/kumo/components/table";
import { Button } from "@cloudflare/kumo/components/button";
import { ArrowDownIcon } from "@phosphor-icons/react/dist/csr/ArrowDown";
import { ArrowsDownUpIcon } from "@phosphor-icons/react/dist/csr/ArrowsDownUp";
import { ArrowUpIcon } from "@phosphor-icons/react/dist/csr/ArrowUp";
import { TrashIcon } from "@phosphor-icons/react/dist/csr/Trash";
import {
  columnFilteringFeature,
  createColumnHelper,
  createFilteredRowModel,
  createSortedRowModel,
  filterFn_includesString,
  globalFilteringFeature,
  rowSortingFeature,
  sortFn_basic,
  tableFeatures,
  useTable,
  type SortingState,
} from "@tanstack/react-table";
import { useMemo, useState, type ReactNode, type RefObject } from "react";

import { payloadPreview, relativeTime } from "@mokmok/web-consumer";
import { instantMillis, type Event } from "@mokmok/web-protocol";

import { InlineCopyText } from "@cloudflare/kumo/components/inline-copy-text";

import { clockTime, isoTime } from "../lib/time.ts";

const eventFeatures = tableFeatures({
  columnFilteringFeature,
  globalFilteringFeature,
  rowSortingFeature,
  sortedRowModel: createSortedRowModel(),
  filteredRowModel: createFilteredRowModel(),
  sortFns: { basic: sortFn_basic },
  filterFns: { includesString: filterFn_includesString },
});

const helper = createColumnHelper<typeof eventFeatures, Event>();

const ReceivedCell = ({ millis, nowMs }: { readonly millis: number; readonly nowMs: number }) => (
  <time className="cell-time" dateTime={isoTime(millis)} title={isoTime(millis)}>
    <span className="cell-time-relative">{relativeTime(nowMs, millis)}</span>
    <span className="cell-time-clock">{clockTime(millis)}</span>
  </time>
);

const SortGlyph = ({ direction }: { readonly direction: false | "asc" | "desc" }) => {
  if (direction === "asc") return <ArrowUpIcon aria-hidden="true" size={12} />;
  if (direction === "desc") return <ArrowDownIcon aria-hidden="true" size={12} />;
  return <ArrowsDownUpIcon aria-hidden="true" size={12} />;
};

const ariaSort = (direction: false | "asc" | "desc"): "ascending" | "descending" | "none" => {
  if (direction === "asc") return "ascending";
  if (direction === "desc") return "descending";
  return "none";
};

interface EventTableProps {
  readonly events: readonly Event[];
  readonly nowMs: number;
  readonly following: boolean;
  readonly onFollowingChange: (following: boolean) => void;
  readonly onClear: () => void;
  readonly onSelect: (event: Event) => void;
  readonly emptyState: ReactNode;
  readonly scrollRef?: RefObject<HTMLDivElement | null>;
}

export const EventTable = ({
  events,
  nowMs,
  following,
  onFollowingChange,
  onClear,
  onSelect,
  emptyState,
  scrollRef,
}: EventTableProps) => {
  const [filter, setFilter] = useState("");

  const columns = useMemo(
    () =>
      helper.columns([
        helper.accessor("seq", {
          header: "Seq",
          sortFn: sortFn_basic,
          cell: (info) => <span className="cell-mono cell-seq">{info.getValue()}</span>,
        }),
        helper.accessor((row) => instantMillis(row.at), {
          id: "received",
          header: "Received",
          sortFn: sortFn_basic,
          cell: (info) => <ReceivedCell millis={info.getValue()} nowMs={nowMs} />,
        }),
        helper.accessor("id", {
          header: "Event id",
          enableSorting: false,
          cell: (info) => (
            <InlineCopyText
              variant="mono"
              value={info.getValue()}
              labels={{ copyAction: "Copy event id", copied: "Event id copied" }}
            >
              {info.getValue()}
            </InlineCopyText>
          ),
        }),
        helper.accessor((row) => payloadPreview(row.payload, 96), {
          id: "preview",
          header: "Payload preview",
          enableSorting: false,
          cell: (info) => <span className="preview">{info.getValue()}</span>,
        }),
      ]),
    [nowMs],
  );

  const table = useTable({
    features: eventFeatures,
    columns,
    data: events,
    initialState: {
      sorting: [{ id: "seq", desc: true }] satisfies SortingState,
      globalFilter: "",
    },
    enableGlobalFilter: true,
    globalFilterFn: filterFn_includesString,
  });

  const rows = table.getRowModel().rows;
  const hasEvents = events.length > 0;

  return (
    <div className="table-block">
      <div className="table-toolbar">
        <div className="filter-field">
          <Input
            label="Filter events"
            placeholder="Filter by seq, event id, or payload…"
            size="sm"
            value={filter}
            onChange={(event) => {
              setFilter(event.target.value);
              table.setGlobalFilter(event.target.value);
            }}
          />
        </div>
        <Switch
          label="Pause follow"
          checked={!following}
          onCheckedChange={(paused) => onFollowingChange(!paused)}
        />
        <Button
          variant="secondary"
          size="sm"
          icon={TrashIcon}
          onClick={onClear}
          disabled={!hasEvents}
        >
          Clear view
        </Button>
      </div>

      {hasEvents ? (
        <LayerCard className="table-card">
          <div className="table-scroll" ref={scrollRef}>
            <Table>
              <Table.Header>
                {table.getHeaderGroups().map((group) => (
                  <Table.Row key={group.id}>
                    {group.headers.map((header) => {
                      const direction = header.column.getIsSorted();
                      const canSort = header.column.getCanSort();
                      return (
                        <Table.Head
                          key={header.id}
                          aria-sort={canSort ? ariaSort(direction) : undefined}
                        >
                          {header.isPlaceholder ? null : canSort ? (
                            <button
                              type="button"
                              className="sort-button"
                              onClick={header.column.getToggleSortingHandler()}
                            >
                              <table.FlexRender header={header} />
                              <SortGlyph direction={direction} />
                            </button>
                          ) : (
                            <table.FlexRender header={header} />
                          )}
                        </Table.Head>
                      );
                    })}
                  </Table.Row>
                ))}
              </Table.Header>
              <Table.Body>
                {rows.map((row) => (
                  <Table.Row
                    key={row.id}
                    className="event-row"
                    tabIndex={0}
                    aria-label={`Open event ${row.original.seq}`}
                    onClick={() => onSelect(row.original)}
                    onKeyDown={(event) => {
                      if (event.key !== "Enter" && event.key !== " ") return;
                      event.preventDefault();
                      onSelect(row.original);
                    }}
                  >
                    {row.getAllCells().map((cell) => (
                      <Table.Cell key={cell.id}>
                        <table.FlexRender cell={cell} />
                      </Table.Cell>
                    ))}
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          </div>
        </LayerCard>
      ) : (
        emptyState
      )}

      {hasEvents && rows.length === 0 ? (
        <div className="table-empty">
          <p className="table-empty-text">No events match “{filter}”.</p>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => {
              setFilter("");
              table.setGlobalFilter("");
            }}
          >
            Clear filter
          </Button>
        </div>
      ) : null}
    </div>
  );
};
