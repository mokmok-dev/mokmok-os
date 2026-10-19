import { Button } from "@cloudflare/kumo/components/button";
import { Dialog } from "@cloudflare/kumo/components/dialog";
import { InlineCopyText } from "@cloudflare/kumo/components/inline-copy-text";
import { Badge } from "@cloudflare/kumo/components/badge";
import { CheckIcon } from "@phosphor-icons/react/dist/csr/Check";
import { CopyIcon } from "@phosphor-icons/react/dist/csr/Copy";
import { XIcon } from "@phosphor-icons/react/dist/csr/X";
import { useEffect, useState } from "react";

import { formatBytes, payloadBytes } from "@mokmok/web-consumer";
import { instantMillis, type Event } from "@mokmok/web-protocol";

import { clockTime, isoTime } from "../lib/time.ts";

interface EventDetailDialogProps {
  readonly event: Event | null;
  readonly epoch: string | null;
  readonly onClose: () => void;
}

export const EventDetailDialog = ({ event, epoch, onClose }: EventDetailDialogProps) => {
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    setCopied(false);
  }, [event?.id]);

  const payload = event === null ? "" : JSON.stringify(event.payload, null, 2);
  const millis = event === null ? 0 : instantMillis(event.at);
  const clipboardAvailable = typeof navigator !== "undefined" && navigator.clipboard !== undefined;

  return (
    <Dialog.Root
      open={event !== null}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <Dialog className="event-dialog" size="xl">
        <div className="dialog-heading">
          <div>
            <Dialog.Title className="dialog-title">
              {event === null ? "Event" : `Event #${event.seq}`}
            </Dialog.Title>
            {event === null ? null : (
              <Dialog.Description className="dialog-subtitle">
                {clockTime(millis)} · {isoTime(millis)}
              </Dialog.Description>
            )}
          </div>
          <Dialog.Close
            aria-label="Close event details"
            render={(props) => (
              <Button {...props} variant="ghost" shape="square" icon={XIcon} aria-label="Close event details" />
            )}
          />
        </div>

        {event === null ? null : (
          <div className="dialog-body">
            <dl className="meta-grid">
              <dt>Event id</dt>
              <dd>
                <InlineCopyText
                  value={event.id}
                  labels={{ copyAction: "Copy event id", copied: "Event id copied" }}
                >
                  {event.id}
                </InlineCopyText>
              </dd>
              <dt>Topic</dt>
              <dd className="meta-value mono">{event.topic}</dd>
              <dt>Sequence</dt>
              <dd className="meta-value mono">{event.seq}</dd>
              <dt>Epoch</dt>
              <dd className="meta-value mono">{epoch ?? "unknown"}</dd>
              <dt>Payload size</dt>
              <dd className="meta-value">
                {formatBytes(payloadBytes(event.payload))}{" "}
                <Badge variant="secondary">{payloadType(event.payload)}</Badge>
              </dd>
            </dl>

            <div className="payload-heading">
              <h3 className="section-title">Payload</h3>
              <Button
                variant="secondary"
                size="sm"
                icon={copied ? CheckIcon : CopyIcon}
                disabled={!clipboardAvailable}
                title={clipboardAvailable ? undefined : "Clipboard access is unavailable"}
                onClick={() => {
                  void navigator.clipboard
                    .writeText(payload)
                    .then(() => setCopied(true))
                    .catch(() => setCopied(false));
                }}
              >
                {copied ? "Copied" : "Copy JSON"}
              </Button>
            </div>
            <pre className="payload-pre" tabIndex={0} aria-label="Event payload">
              {payload}
            </pre>
          </div>
        )}
      </Dialog>
    </Dialog.Root>
  );
};

const payloadType = (payload: unknown): string => {
  if (payload === null) return "null";
  if (Array.isArray(payload)) return "array";
  return typeof payload;
};
