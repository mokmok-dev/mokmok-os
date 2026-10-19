import type { ConnectionPhase } from "@mokmok/web-protocol";

export interface BridgeOptions {
  readonly socketPath: string;
  readonly topic: string;
  readonly subscriberFile: string;
  readonly host?: string;
  readonly port: number;
  readonly staticDir?: string;
}

export interface BridgeConfig {
  readonly socketPath: string;
  readonly topic: string;
  readonly subscriberFile: string;
  readonly host: string;
  readonly port: number;
  readonly staticDir: string;
}

export interface BridgeHandle {
  readonly port: number;
  readonly subscriberId: string;
  close(): Promise<void>;
}

export interface BridgeState {
  readonly status: ConnectionPhase;
  readonly eventsDelivered: number;
  readonly lastSeq: number | null;
  readonly pendingBatch: string | null;
  readonly connectedAtMs: number | null;
}
