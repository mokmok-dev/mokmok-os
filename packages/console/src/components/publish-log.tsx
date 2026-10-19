import { Badge } from "@cloudflare/kumo/components/badge";
import { InlineCopyText } from "@cloudflare/kumo/components/inline-copy-text";
import { LayerCard } from "@cloudflare/kumo/components/layer-card";
import { Table } from "@cloudflare/kumo/components/table";
import { instantMillis } from "@mokmok/web-protocol";
import { relativeTime, type PublishFailure, type Receipt } from "@mokmok/web-consumer";

import { clockTime } from "../lib/time.ts";

interface PublishLogProps {
  readonly receipts: readonly Receipt[];
  readonly failures: readonly PublishFailure[];
  readonly nowMs: number;
  readonly submittedAt: ReadonlyMap<string, number>;
}

const RequestIdCell = ({ requestId }: { readonly requestId: string }) => (
  <InlineCopyText
    value={requestId}
    variant="mono-secondary"
    labels={{ copyAction: "Copy request id", copied: "Request id copied" }}
  >
    {requestId}
  </InlineCopyText>
);

export const PublishLog = ({ receipts, failures, nowMs, submittedAt }: PublishLogProps) => {
  const confirmed = new Set(receipts.map((receipt) => receipt.requestId));
  const unresolvedFailures = failures.filter((failure) => !confirmed.has(failure.requestId));

  return (
    <div className="publish-log">
      <section className="log-section" aria-labelledby="receipts-heading">
        <h2 id="receipts-heading" className="section-title">
          Recent receipts
        </h2>
        {receipts.length === 0 ? (
          <p className="list-empty">No publish has been confirmed yet.</p>
        ) : (
          <LayerCard className="table-card">
            <Table>
              <Table.Header>
                <Table.Row>
                  <Table.Head>Request id</Table.Head>
                  <Table.Head className="cell-right">Seq</Table.Head>
                  <Table.Head className="cell-right">Confirmed</Table.Head>
                  <Table.Head className="cell-right">
                    <span className="sr-only">Status</span>
                  </Table.Head>
                </Table.Row>
              </Table.Header>
              <Table.Body>
                {receipts.map((receipt) => (
                  <Table.Row key={receipt.requestId}>
                    <Table.Cell>
                      <RequestIdCell requestId={receipt.requestId} />
                    </Table.Cell>
                    <Table.Cell className="cell-right cell-mono">{receipt.seq}</Table.Cell>
                    <Table.Cell className="cell-right cell-mono">
                      <span title={clockTime(instantMillis(receipt.at))}>
                        {relativeTime(nowMs, instantMillis(receipt.at))}
                      </span>
                    </Table.Cell>
                    <Table.Cell className="cell-right">
                      <Badge variant="success">Confirmed</Badge>
                    </Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          </LayerCard>
        )}
      </section>

      <section className="log-section" aria-labelledby="failures-heading">
        <h2 id="failures-heading" className="section-title">
          Recent failures
        </h2>
        {unresolvedFailures.length === 0 ? (
          <p className="list-empty">No publish has failed in this tab.</p>
        ) : (
          <LayerCard className="table-card">
            <Table>
              <Table.Header>
                <Table.Row>
                  <Table.Head>Request id</Table.Head>
                  <Table.Head>Detail</Table.Head>
                  <Table.Head className="cell-right">Attempted</Table.Head>
                  <Table.Head className="cell-right">Kind</Table.Head>
                </Table.Row>
              </Table.Header>
              <Table.Body>
                {unresolvedFailures.map((failure) => {
                  const submitted = submittedAt.get(failure.requestId);
                  return (
                    <Table.Row key={failure.requestId}>
                      <Table.Cell>
                        <RequestIdCell requestId={failure.requestId} />
                      </Table.Cell>
                      <Table.Cell className="failure-detail">{failure.detail}</Table.Cell>
                      <Table.Cell className="cell-right cell-mono">
                        {submitted === undefined ? "—" : relativeTime(nowMs, submitted)}
                      </Table.Cell>
                      <Table.Cell className="cell-right">
                        {failure.indeterminate ? (
                          <Badge variant="warning">Indeterminate</Badge>
                        ) : (
                          <Badge variant="error">Rejected</Badge>
                        )}
                      </Table.Cell>
                    </Table.Row>
                  );
                })}
              </Table.Body>
            </Table>
          </LayerCard>
        )}
      </section>
    </div>
  );
};
