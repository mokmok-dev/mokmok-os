import { Button } from "@cloudflare/kumo/components/button";
import { InlineCopyText } from "@cloudflare/kumo/components/inline-copy-text";
import { InputArea } from "@cloudflare/kumo/components/input";
import { Meter } from "@cloudflare/kumo/components/meter";
import { LightningIcon } from "@phosphor-icons/react/dist/csr/Lightning";
import { PaperPlaneTiltIcon } from "@phosphor-icons/react/dist/csr/PaperPlaneTilt";
import type { Json } from "effect/Schema";
import { useMemo, useState } from "react";

import { formatBytes, MAX_PAYLOAD_BYTES, parsePayload } from "@mokmok/web-consumer";

const SAMPLE_PAYLOAD = `{
  "kind": "console.demo",
  "message": "hello from the mokmok web console",
  "level": "info"
}`;

const byteLength = (text: string): number => new TextEncoder().encode(text).length;

interface PayloadComposerProps {
  readonly onPublish: (payload: Json) => void;
  readonly publishing: boolean;
  readonly lastRequestId: string | null;
}

export const PayloadComposer = ({ onPublish, publishing, lastRequestId }: PayloadComposerProps) => {
  const [text, setText] = useState("");
  const parsed = useMemo(() => parsePayload(text), [text]);
  const bytes = useMemo(() => byteLength(text), [text]);
  const blank = text.trim().length === 0;
  const canPublish = parsed.ok && !publishing;

  const validation = blank
    ? "Paste any JSON value. Objects, arrays, strings, numbers, booleans, and null are all accepted."
    : parsed.ok
      ? "Valid JSON — ready to publish."
      : parsed.message;

  return (
    <section className="composer" aria-labelledby="composer-heading">
      <div className="composer-heading">
        <div>
          <h2 id="composer-heading" className="section-title">
            Compose a payload
          </h2>
          <p className="section-hint">
            Publish a single event to this console&apos;s topic. The bridge signs it with the bus
            sequence number.
          </p>
        </div>
        <Button
          variant="secondary"
          size="sm"
          icon={LightningIcon}
          onClick={() => setText(SAMPLE_PAYLOAD)}
        >
          Use a sample payload
        </Button>
      </div>

      <InputArea
        label="Payload (JSON)"
        className="payload-input"
        rows={12}
        spellCheck={false}
        placeholder={SAMPLE_PAYLOAD}
        value={text}
        onChange={(event) => setText(event.target.value)}
        error={blank || parsed.ok ? undefined : parsed.message}
        description={blank || parsed.ok ? validation : undefined}
      />
      <Meter
        label="UTF-8 size"
        value={Math.min(bytes, MAX_PAYLOAD_BYTES)}
        max={MAX_PAYLOAD_BYTES}
        customValue={`${formatBytes(bytes)} / ${formatBytes(MAX_PAYLOAD_BYTES)}`}
      />

      <div className="composer-actions">
        <Button
          variant="primary"
          icon={PaperPlaneTiltIcon}
          onClick={() => {
            if (!parsed.ok) return;
            onPublish(parsed.payload);
          }}
          disabled={!canPublish}
          loading={publishing}
        >
          Publish event
        </Button>
        <Button
          variant="secondary"
          onClick={() => setText("")}
          disabled={blank || publishing}
        >
          Clear
        </Button>
      </div>

      <div className="request-id-row">
        <span className="meta-label">Request id</span>
        {lastRequestId === null ? (
          <span className="meta-muted">Generated when you publish.</span>
        ) : (
          <InlineCopyText
            value={lastRequestId}
            variant="mono"
            labels={{ copyAction: "Copy request id", copied: "Request id copied" }}
          >
            {lastRequestId}
          </InlineCopyText>
        )}
      </div>
    </section>
  );
};
