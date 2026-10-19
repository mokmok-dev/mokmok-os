import { Schema } from "effect";

import { At, Cursor, Event, EventId, Lapsed, Seq, SubscriberId, Topic, Window } from "./wire.ts";

export const ConnectionPhase = Schema.Literals(["connecting", "live", "refused", "offline"]);
export type ConnectionPhase = Schema.Schema.Type<typeof ConnectionPhase>;

const RequestId = Schema.String.pipe(Schema.check(Schema.isNonEmpty()));
const BatchId = Schema.String.pipe(Schema.check(Schema.isNonEmpty()));

export const HelloFrame = Schema.Struct({
  op: Schema.Literal("hello"),
  topic: Topic,
  subscriber: SubscriberId,
});
export type HelloFrame = Schema.Schema.Type<typeof HelloFrame>;

export const StatusFrame = Schema.Struct({
  op: Schema.Literal("status"),
  connection: ConnectionPhase,
  detail: Schema.NullOr(Schema.String),
});
export type StatusFrame = Schema.Schema.Type<typeof StatusFrame>;

export const BatchFrame = Schema.Struct({
  op: Schema.Literal("batch"),
  batch_id: BatchId,
  window: Window,
  lapsed: Schema.NullOr(Lapsed),
  events: Schema.Array(Event),
});
export type BatchFrame = Schema.Schema.Type<typeof BatchFrame>;

export const PublishedFrame = Schema.Struct({
  op: Schema.Literal("published"),
  request_id: RequestId,
  id: EventId,
  seq: Seq,
  at: At,
});
export type PublishedFrame = Schema.Schema.Type<typeof PublishedFrame>;

export const PublishFailedFrame = Schema.Struct({
  op: Schema.Literal("publish_failed"),
  request_id: RequestId,
  detail: Schema.String,
  indeterminate: Schema.Boolean,
});
export type PublishFailedFrame = Schema.Schema.Type<typeof PublishFailedFrame>;

export const AckedFrame = Schema.Struct({
  op: Schema.Literal("acked"),
  through: Cursor,
});
export type AckedFrame = Schema.Schema.Type<typeof AckedFrame>;

export const BridgeServerFrame = Schema.Union([
  HelloFrame,
  StatusFrame,
  BatchFrame,
  PublishedFrame,
  PublishFailedFrame,
  AckedFrame,
]);
export type BridgeServerFrame = Schema.Schema.Type<typeof BridgeServerFrame>;

export const AckFrame = Schema.Struct({
  op: Schema.Literal("ack"),
  batch_id: BatchId,
  through: Cursor,
});
export type AckFrame = Schema.Schema.Type<typeof AckFrame>;

export const PublishFrame = Schema.Struct({
  op: Schema.Literal("publish"),
  request_id: RequestId,
  payload: Schema.Json,
});
export type PublishFrame = Schema.Schema.Type<typeof PublishFrame>;

export const BridgeClientFrame = Schema.Union([AckFrame, PublishFrame]);
export type BridgeClientFrame = Schema.Schema.Type<typeof BridgeClientFrame>;

const Count = Schema.Number.pipe(
  Schema.check(Schema.isInt(), Schema.isGreaterThanOrEqualTo(0)),
);

export const HealthResponse = Schema.Struct({
  status: ConnectionPhase,
  topic: Topic,
  subscriber: SubscriberId,
  socket: Schema.String,
  events_delivered: Count,
  last_seq: Schema.NullOr(Seq),
  pending_batch: Schema.NullOr(Schema.String),
  connected_at_ms: Schema.NullOr(Count),
});
export type HealthResponse = Schema.Schema.Type<typeof HealthResponse>;

export const decodeHealthResponse = (value: unknown): HealthResponse | null => {
  const decoded = Schema.decodeUnknownOption(HealthResponse)(value);

  return decoded._tag === "Some" ? decoded.value : null;
};

const HealthResponseJson = Schema.fromJsonString(HealthResponse);

export const parseHealthResponse = (text: string): HealthResponse | null => {
  const decoded = Schema.decodeUnknownOption(HealthResponseJson)(text);

  return decoded._tag === "Some" ? decoded.value : null;
};

export const decodeBridgeServerFrame = (value: unknown): BridgeServerFrame | null => {
  const decoded = Schema.decodeUnknownOption(BridgeServerFrame)(value);

  return decoded._tag === "Some" ? decoded.value : null;
};

export const decodeBridgeClientFrame = (value: unknown): BridgeClientFrame | null => {
  const decoded = Schema.decodeUnknownOption(BridgeClientFrame)(value);

  return decoded._tag === "Some" ? decoded.value : null;
};

const BridgeServerFrameJson = Schema.fromJsonString(BridgeServerFrame);
const BridgeClientFrameJson = Schema.fromJsonString(BridgeClientFrame);

export const parseBridgeServerFrame = (text: string): BridgeServerFrame | null => {
  const decoded = Schema.decodeUnknownOption(BridgeServerFrameJson)(text);

  return decoded._tag === "Some" ? decoded.value : null;
};

export const parseBridgeClientFrame = (text: string): BridgeClientFrame | null => {
  const decoded = Schema.decodeUnknownOption(BridgeClientFrameJson)(text);

  return decoded._tag === "Some" ? decoded.value : null;
};

export const encodeBridgeClientFrame = (frame: BridgeClientFrame): string =>
  JSON.stringify(frame);

export const describeConnection = (phase: ConnectionPhase): string => {
  switch (phase) {
    case "connecting":
      return "Connecting to the bridge";
    case "live":
      return "Streaming from the bus";
    case "refused":
      return "The bus refused the subscription";
    case "offline":
      return "The bridge is offline";
  }
};
