import { useMutation } from "@tanstack/react-query";
import { useKumoToastManager } from "@cloudflare/kumo/components/toast";
import { PaperPlaneTiltIcon } from "@phosphor-icons/react/dist/csr/PaperPlaneTilt";
import type { Json } from "effect/Schema";
import { useState } from "react";

import { describeConnection, instantMillis } from "@mokmok/web-protocol";

import { ConnectionBadge } from "../components/connection-badge.tsx";
import { PayloadComposer } from "../components/payload-composer.tsx";
import { PublishLog } from "../components/publish-log.tsx";
import { useConsole } from "../lib/console-context.tsx";
import { clockTime } from "../lib/time.ts";

export const PublishPage = () => {
  const { state, publish, nowMs } = useConsole();
  const toast = useKumoToastManager();
  const [lastRequestId, setLastRequestId] = useState<string | null>(null);
  const [submittedAt] = useState(() => new Map<string, number>());

  const mutation = useMutation({
    mutationFn: (input: { requestId: string; payload: Json }) =>
      publish(input.requestId, input.payload),
    onSuccess: (outcome) => {
      if (outcome.ok) {
        toast.add({
          variant: "success",
          title: `Published event #${outcome.published.seq}`,
          description: `Confirmed by the bus at ${clockTime(instantMillis(outcome.published.at))}.`,
        });
        return;
      }
      toast.add({
        variant: outcome.failure.indeterminate ? "warning" : "error",
        title: "Publish failed",
        description: outcome.failure.indeterminate
          ? `${outcome.failure.detail}. The bus may still have accepted the event.`
          : outcome.failure.detail,
      });
    },
    onError: (error) => {
      toast.add({
        variant: "error",
        title: "Publish failed",
        description: error instanceof Error ? error.message : String(error),
      });
    },
  });

  const handlePublish = (payload: Json) => {
    const requestId = crypto.randomUUID();
    submittedAt.set(requestId, Date.now());
    setLastRequestId(requestId);
    mutation.mutate({ requestId, payload });
  };

  return (
    <div className="page">
      <header className="page-header">
        <div className="page-title-group">
          <p className="eyebrow">mokmok web</p>
          <h1 className="page-title">Publish</h1>
          <p className="page-subtitle">
            Write one event to topic <span className="mono">{state.topic}</span> and watch the
            receipt come back from the bus.
          </p>
        </div>
        <div className="status-line" role="status" aria-live="polite">
          <ConnectionBadge phase={state.phase} />
          <span className="status-text">{describeConnection(state.phase)}</span>
          {state.detail === null ? null : <span className="status-detail">{state.detail}</span>}
        </div>
      </header>

      <div className="publish-hero">
        <PaperPlaneTiltIcon size={20} className="publish-hero-icon" aria-hidden="true" />
        <p className="publish-hero-text">
          Every publish is correlated by request id. A receipt means the bus assigned the event a
          sequence number; a failure marked indeterminate means the bridge lost the answer before it
          could confirm either way.
        </p>
      </div>

      <PayloadComposer
        onPublish={handlePublish}
        publishing={mutation.isPending}
        lastRequestId={lastRequestId}
      />

      <PublishLog
        receipts={state.receipts}
        failures={state.failures}
        nowMs={nowMs}
        submittedAt={submittedAt}
      />
    </div>
  );
};
