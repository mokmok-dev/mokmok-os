import type {
  At,
  BatchFrame,
  ConnectionPhase,
  Cursor,
  Event,
  Lapsed,
  PublishedFrame,
  PublishFailedFrame,
  Window,
} from "@mokmok/web-protocol";

export const EVENT_CAP = 500;

const RECEIPT_CAP = 100;
const FAILURE_CAP = 100;

export interface Receipt {
  requestId: string;
  id: string;
  seq: number;
  at: At;
}

export interface PublishFailure {
  requestId: string;
  detail: string;
  indeterminate: boolean;
}

export interface PendingBatch {
  batchId: string;
  through: Cursor;
}

export interface ConsumerState {
  topic: string;
  phase: ConnectionPhase;
  detail: string | null;
  subscriber: string | null;
  window: Window | null;
  checkpoint: Cursor | null;
  pending: PendingBatch | null;
  events: readonly Event[];
  lapsed: Lapsed | null;
  receipts: readonly Receipt[];
  failures: readonly PublishFailure[];
  evicted: number;
}

export type ConsumerMessage =
  | { type: "hello"; topic: string; subscriber: string }
  | { type: "status"; connection: ConnectionPhase; detail: string | null }
  | { type: "batch"; frame: BatchFrame }
  | { type: "acked"; through: Cursor }
  | { type: "published"; frame: PublishedFrame }
  | { type: "publish_failed"; frame: PublishFailedFrame }
  | { type: "clear_events" };

type HelloMessage = Extract<ConsumerMessage, { type: "hello" }>;
type StatusMessage = Extract<ConsumerMessage, { type: "status" }>;

export const initialState = (topic: string): ConsumerState => ({
  topic,
  phase: "connecting",
  detail: null,
  subscriber: null,
  window: null,
  checkpoint: null,
  pending: null,
  events: [],
  lapsed: null,
  receipts: [],
  failures: [],
  evicted: 0,
});

export const batchThrough = (frame: BatchFrame): Cursor => {
  const last = frame.events[frame.events.length - 1];

  if (last !== undefined) {
    return { epoch: frame.window.epoch, seq: last.seq + 1 };
  }

  if (frame.lapsed !== null) {
    return { epoch: frame.window.epoch, seq: frame.lapsed.from };
  }

  return { epoch: frame.window.epoch, seq: frame.window.next };
};

const lastAppliedSeq = (events: readonly Event[]): number | null => {
  const last = events[events.length - 1];

  return last === undefined ? null : last.seq;
};

const acceptEvents = (
  held: readonly Event[],
  incoming: readonly Event[],
): readonly Event[] => {
  const accepted: Event[] = [];
  let highWater = lastAppliedSeq(held);

  for (const event of incoming) {
    if (highWater === null || event.seq > highWater) {
      accepted.push(event);
      highWater = event.seq;
    }
  }

  return accepted;
};

const applyBatch = (state: ConsumerState, frame: BatchFrame): ConsumerState => {
  const epochChanged =
    state.window !== null && state.window.epoch !== frame.window.epoch;
  const held = epochChanged ? [] : state.events;
  const combined = [...held, ...acceptEvents(held, frame.events)];
  const retained = Math.min(combined.length, EVENT_CAP);
  const dropped = combined.length - retained;

  return {
    ...state,
    window: frame.window,
    lapsed: frame.lapsed,
    events: combined.slice(combined.length - retained),
    evicted: (epochChanged ? 0 : state.evicted) + dropped,
    pending: {
      batchId: frame.batch_id,
      through: batchThrough(frame),
    },
  };
};

const sameCursor = (left: Cursor, right: Cursor): boolean =>
  left.epoch === right.epoch && left.seq === right.seq;

const advanceCheckpoint = (held: Cursor | null, through: Cursor): Cursor => {
  if (held === null || held.epoch !== through.epoch) {
    return through;
  }

  return { epoch: held.epoch, seq: Math.max(held.seq, through.seq) };
};

const applyAcked = (state: ConsumerState, through: Cursor): ConsumerState => {
  const pending = state.pending;

  if (pending === null) {
    return state;
  }

  if (!sameCursor(pending.through, through)) {
    return state;
  }

  return {
    ...state,
    checkpoint: advanceCheckpoint(state.checkpoint, through),
    pending: null,
  };
};

const applyPublished = (
  state: ConsumerState,
  frame: PublishedFrame,
): ConsumerState => ({
  ...state,
  receipts: [
    {
      requestId: frame.request_id,
      id: frame.id,
      seq: frame.seq,
      at: frame.at,
    },
    ...state.receipts,
  ].slice(0, RECEIPT_CAP),
});

const applyPublishFailed = (
  state: ConsumerState,
  frame: PublishFailedFrame,
): ConsumerState => ({
  ...state,
  failures: [
    {
      requestId: frame.request_id,
      detail: frame.detail,
      indeterminate: frame.indeterminate,
    },
    ...state.failures,
  ].slice(0, FAILURE_CAP),
});

const applyHello = (state: ConsumerState, message: HelloMessage): ConsumerState => ({
  ...state,
  topic: message.topic,
  subscriber: message.subscriber,
});

const applyStatus = (state: ConsumerState, message: StatusMessage): ConsumerState => ({
  ...state,
  phase: message.connection,
  detail: message.detail,
});

const applyClearEvents = (state: ConsumerState): ConsumerState => ({
  ...state,
  events: [],
  evicted: 0,
});

export const reduce = (
  state: ConsumerState,
  message: ConsumerMessage,
): ConsumerState => {
  switch (message.type) {
    case "hello":
      return applyHello(state, message);
    case "status":
      return applyStatus(state, message);
    case "batch":
      return applyBatch(state, message.frame);
    case "acked":
      return applyAcked(state, message.through);
    case "published":
      return applyPublished(state, message.frame);
    case "publish_failed":
      return applyPublishFailed(state, message.frame);
    case "clear_events":
      return applyClearEvents(state);
  }
};
