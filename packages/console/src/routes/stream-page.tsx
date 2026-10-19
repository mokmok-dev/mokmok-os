import { Banner } from "@cloudflare/kumo/components/banner";
import { Button } from "@cloudflare/kumo/components/button";
import { Empty } from "@cloudflare/kumo/components/empty";
import { Loader } from "@cloudflare/kumo/components/loader";
import { PlugIcon } from "@phosphor-icons/react/dist/csr/Plug";
import { WaveformIcon } from "@phosphor-icons/react/dist/csr/Waveform";
import { WarningIcon } from "@phosphor-icons/react/dist/csr/Warning";
import { WarningCircleIcon } from "@phosphor-icons/react/dist/csr/WarningCircle";
import { useLayoutEffect, useRef, useState } from "react";

import { describeConnection, type Event } from "@mokmok/web-protocol";

import { ConnectionBadge } from "../components/connection-badge.tsx";
import { EventDetailDialog } from "../components/event-detail-dialog.tsx";
import { EventTable } from "../components/event-table.tsx";
import { useConsole } from "../lib/console-context.tsx";

const Stat = ({ label, value }: { readonly label: string; readonly value: string | number }) => (
  <div className="stat">
    <span className="stat-label">{label}</span>
    <span className="stat-value">{value}</span>
  </div>
);

const shortEpoch = (epoch: string | null): string => (epoch === null ? "no epoch" : epoch.slice(0, 8));

export const StreamPage = () => {
  const { state, storageError, retryPersist, clearEvents, nowMs } = useConsole();
  const [following, setFollowing] = useState(true);
  const [selected, setSelected] = useState<Event | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const newestSeq = state.events.at(-1)?.seq ?? null;

  useLayoutEffect(() => {
    if (following && scrollRef.current !== null) scrollRef.current.scrollTop = 0;
  }, [following, newestSeq]);

  const emptyState = (
    <Empty
      icon={<WaveformIcon size={48} className="empty-icon" />}
      title={state.phase === "live" ? "Waiting for the first event" : "The stream is not live yet"}
      description={
        state.phase === "live"
          ? "The bridge is streaming. Publish an event or run a mokmok agent to see it here."
          : `${describeConnection(state.phase)}. Events will appear automatically once the subscription is live.`
      }
      contents={state.phase === "connecting" ? <Loader size="sm" /> : undefined}
    />
  );

  return (
    <div className="page">
      <header className="page-header">
        <div className="page-title-group">
          <p className="eyebrow">mokmok web</p>
          <h1 className="page-title">Stream</h1>
          <p className="page-subtitle">
            Topic <span className="mono">{state.topic}</span> · epoch{" "}
            <span className="mono">{shortEpoch(state.window?.epoch ?? null)}</span>
          </p>
        </div>
        <div className="status-line" role="status" aria-live="polite">
          <ConnectionBadge phase={state.phase} />
          <span className="status-text">{describeConnection(state.phase)}</span>
          {state.detail === null ? null : <span className="status-detail">{state.detail}</span>}
        </div>
      </header>

      <div className="stat-row">
        <Stat label="Events in view" value={state.events.length} />
        <Stat label="Last seq" value={newestSeq ?? "—"} />
        <Stat label="Checkpoint" value={state.checkpoint?.seq ?? "—"} />
        <Stat label="Evicted locally" value={state.evicted} />
      </div>

      <div className="banner-stack">
        {state.phase === "refused" ? (
          <Banner
            variant="error"
            icon={<WarningCircleIcon weight="fill" />}
            title="The bus refused this subscription"
            description={state.detail ?? describeConnection("refused")}
          />
        ) : null}

        {state.phase === "offline" && storageError === null ? (
          <Banner
            variant="alert"
            icon={<PlugIcon weight="fill" />}
            title="The bridge is offline"
            description={`${state.detail ?? describeConnection("offline")}. The console reconnects automatically with capped exponential backoff.`}
          />
        ) : null}

        {state.lapsed === null ? null : (
          <Banner
            variant="alert"
            icon={<WarningIcon weight="fill" />}
            title="The stream lapsed"
            description={`The bus could not replay from #${state.lapsed.held.seq} and resumed at #${state.lapsed.from}. Events between those positions are missing from this view.`}
          />
        )}

        {storageError === null ? null : (
          <Banner
            variant="error"
            icon={<WarningCircleIcon weight="fill" />}
            title="Checkpoint not saved"
            description={`${storageError}. The console stopped acknowledging batches so a crash cannot lose events.`}
            action={
              <Button variant="secondary" size="sm" onClick={retryPersist}>
                Retry saving
              </Button>
            }
          />
        )}

        {state.evicted === 0 ? null : (
          <Banner
            variant="secondary"
            icon={<WaveformIcon weight="fill" />}
            title="Local buffer trimmed"
            description={`${state.evicted} older ${state.evicted === 1 ? "event was" : "events were"} dropped from this tab's capped table. The durable checkpoint is unaffected.`}
          />
        )}
      </div>

      <EventTable
        events={state.events}
        nowMs={nowMs}
        following={following}
        onFollowingChange={setFollowing}
        onClear={clearEvents}
        onSelect={setSelected}
        emptyState={emptyState}
        scrollRef={scrollRef}
      />

      <EventDetailDialog
        event={selected}
        epoch={state.window?.epoch ?? null}
        onClose={() => setSelected(null)}
      />
    </div>
  );
};
