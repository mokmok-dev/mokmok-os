import { Schema } from "effect";

const UUID_SOURCE = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
const UUID_PATTERN = new RegExp(`^${UUID_SOURCE}$`);
const TOPIC_PATTERN = new RegExp(`^(global|(agent|session|execution):${UUID_SOURCE})$`);

const MAX_SAFE = Number.MAX_SAFE_INTEGER;
const NANOS_PER_SECOND = 1_000_000_000;
const MILLIS_PER_SECOND = 1000;
const NANOS_PER_MILLI = 1_000_000;

export const SubscriberId = Schema.String.pipe(Schema.check(Schema.isPattern(UUID_PATTERN)));
export type SubscriberId = Schema.Schema.Type<typeof SubscriberId>;

export const EventId = Schema.String.pipe(Schema.check(Schema.isPattern(UUID_PATTERN)));
export type EventId = Schema.Schema.Type<typeof EventId>;

export const LogEpoch = Schema.String.pipe(Schema.check(Schema.isPattern(UUID_PATTERN)));
export type LogEpoch = Schema.Schema.Type<typeof LogEpoch>;

export const Topic = Schema.String.pipe(Schema.check(Schema.isPattern(TOPIC_PATTERN)));
export type Topic = Schema.Schema.Type<typeof Topic>;

export const Seq = Schema.Number.pipe(
  Schema.check(
    Schema.isInt(),
    Schema.isGreaterThanOrEqualTo(0),
    Schema.isLessThanOrEqualTo(MAX_SAFE),
  ),
);
export type Seq = Schema.Schema.Type<typeof Seq>;

export const At = Schema.Struct({
  secs_since_epoch: Schema.Number.pipe(
    Schema.check(
      Schema.isInt(),
      Schema.isGreaterThanOrEqualTo(0),
      Schema.isLessThanOrEqualTo(MAX_SAFE),
    ),
  ),
  nanos_since_epoch: Schema.Number.pipe(
    Schema.check(
      Schema.isInt(),
      Schema.isGreaterThanOrEqualTo(0),
      Schema.isLessThanOrEqualTo(NANOS_PER_SECOND - 1),
    ),
  ),
});
export type At = Schema.Schema.Type<typeof At>;

export const Cursor = Schema.Struct({
  epoch: LogEpoch,
  seq: Seq,
});
export type Cursor = Schema.Schema.Type<typeof Cursor>;

export const Window = Schema.Struct({
  epoch: LogEpoch,
  first: Seq,
  next: Seq,
});
export type Window = Schema.Schema.Type<typeof Window>;

export const Lapsed = Schema.Struct({
  held: Cursor,
  from: Seq,
});
export type Lapsed = Schema.Schema.Type<typeof Lapsed>;

export const Event = Schema.Struct({
  id: EventId,
  topic: Topic,
  seq: Seq,
  at: At,
  payload: Schema.Json,
});
export type Event = Schema.Schema.Type<typeof Event>;

export const Refusal = Schema.Union([
  Schema.Literal("topic_limit"),
  Schema.Literal("connection_limit"),
  Schema.Literal("shutting_down"),
  Schema.Struct({ unresumable: Schema.Struct({ window: Window }) }),
]);
export type Refusal = Schema.Schema.Type<typeof Refusal>;

export const OpenRequest = Schema.Struct({
  op: Schema.Literal("open"),
  topic: Topic,
  subscriber: Schema.NullOr(SubscriberId),
});
export type OpenRequest = Schema.Schema.Type<typeof OpenRequest>;

export const PublishRequest = Schema.Struct({
  op: Schema.Literal("publish"),
  payload: Schema.Json,
});
export type PublishRequest = Schema.Schema.Type<typeof PublishRequest>;

export const PullRequest = Schema.Struct({
  op: Schema.Literal("pull"),
  max: Schema.Number.pipe(Schema.check(Schema.isInt(), Schema.isGreaterThan(0))),
});
export type PullRequest = Schema.Schema.Type<typeof PullRequest>;

export const AckRequest = Schema.Struct({
  op: Schema.Literal("ack"),
  through: Cursor,
});
export type AckRequest = Schema.Schema.Type<typeof AckRequest>;

export const BusRequest = Schema.Union([OpenRequest, PublishRequest, PullRequest, AckRequest]);
export type BusRequest = Schema.Schema.Type<typeof BusRequest>;

export const OpenedResponse = Schema.Struct({
  op: Schema.Literal("opened"),
  window: Window,
});
export type OpenedResponse = Schema.Schema.Type<typeof OpenedResponse>;

export const PublishedResponse = Schema.Struct({
  op: Schema.Literal("published"),
  id: EventId,
  seq: Seq,
  at: At,
});
export type PublishedResponse = Schema.Schema.Type<typeof PublishedResponse>;

export const BatchResponse = Schema.Struct({
  op: Schema.Literal("batch"),
  window: Window,
  lapsed: Schema.NullOr(Lapsed),
  events: Schema.Array(Event),
});
export type BatchResponse = Schema.Schema.Type<typeof BatchResponse>;

export const AckedResponse = Schema.Struct({
  op: Schema.Literal("acked"),
  through: Cursor,
});
export type AckedResponse = Schema.Schema.Type<typeof AckedResponse>;

export const RefusedResponse = Schema.Struct({
  op: Schema.Literal("refused"),
  reason: Refusal,
});
export type RefusedResponse = Schema.Schema.Type<typeof RefusedResponse>;

export const BusResponse = Schema.Union([
  OpenedResponse,
  PublishedResponse,
  BatchResponse,
  AckedResponse,
  RefusedResponse,
]);
export type BusResponse = Schema.Schema.Type<typeof BusResponse>;

export const decodeBusRequest = (value: unknown): BusRequest | null => {
  const decoded = Schema.decodeUnknownOption(BusRequest)(value);

  return decoded._tag === "Some" ? decoded.value : null;
};

export const decodeBusResponse = (value: unknown): BusResponse | null => {
  const decoded = Schema.decodeUnknownOption(BusResponse)(value);

  return decoded._tag === "Some" ? decoded.value : null;
};

export const encodeBusRequest = (request: BusRequest): string => JSON.stringify(request);

const BusResponseJson = Schema.fromJsonString(BusResponse);

export const parseBusResponse = (text: string): BusResponse | null => {
  const decoded = Schema.decodeUnknownOption(BusResponseJson)(text);

  return decoded._tag === "Some" ? decoded.value : null;
};

export const instantMillis = (at: At): number =>
  at.secs_since_epoch * MILLIS_PER_SECOND + Math.floor(at.nanos_since_epoch / NANOS_PER_MILLI);

export const atFromMillis = (millis: number): At => ({
  secs_since_epoch: Math.floor(millis / MILLIS_PER_SECOND),
  nanos_since_epoch: (millis % MILLIS_PER_SECOND) * NANOS_PER_MILLI,
});

const UNIT_REFUSAL_TEXT = {
  topic_limit: "the bus has no room for another topic",
  connection_limit: "the bus is at its connection ceiling",
  shutting_down: "the bus is shutting down",
} as const;

export const describeRefusal = (reason: Refusal): string => {
  if (typeof reason === "string") {
    return UNIT_REFUSAL_TEXT[reason];
  }

  const window = reason.unresumable.window;

  return `the log holds ${window.first}..${window.next} and cannot be read from that cursor`;
};
