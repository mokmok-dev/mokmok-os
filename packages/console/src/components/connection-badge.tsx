import { Badge } from "@cloudflare/kumo/components/badge";
import type { ConnectionPhase } from "@mokmok/web-protocol";

const BADGE_VARIANT: Record<ConnectionPhase, "neutral" | "success" | "error" | "warning"> = {
  connecting: "neutral",
  live: "success",
  refused: "error",
  offline: "warning",
};

export const PHASE_LABEL: Record<ConnectionPhase, string> = {
  connecting: "Connecting",
  live: "Live",
  refused: "Refused",
  offline: "Offline",
};

export const ConnectionBadge = ({ phase }: { readonly phase: ConnectionPhase }) => (
  <Badge appearance="dot" variant={BADGE_VARIANT[phase]}>
    {PHASE_LABEL[phase]}
  </Badge>
);
