import { Schema } from "effect";

import {
  Cursor as CursorSchema,
  Event as EventSchema,
  SubscriberId as SubscriberIdSchema,
} from "@mokmok/web-protocol";

import type { ConsumerState } from "./state.ts";
import { initialState } from "./state.ts";

const STORED_VERSION = 1;

const StoredDocument = Schema.Struct({
  version: Schema.Literal(STORED_VERSION),
  topic: Schema.String,
  subscriber: Schema.NullOr(SubscriberIdSchema),
  checkpoint: Schema.NullOr(CursorSchema),
  events: Schema.Array(EventSchema),
  evicted: Schema.Number.pipe(
    Schema.check(Schema.isInt(), Schema.isGreaterThanOrEqualTo(0)),
  ),
});

type StoredDocument = Schema.Schema.Type<typeof StoredDocument>;

const StoredDocumentJson = Schema.fromJsonString(StoredDocument);

export const hydrate = (raw: string | null, topic: string): ConsumerState => {
  const decoded = Schema.decodeUnknownOption(StoredDocumentJson)(String(raw));

  if (decoded._tag !== "Some") {
    return initialState(topic);
  }

  const stored: StoredDocument = decoded.value;

  if (stored.topic !== topic) {
    return initialState(topic);
  }

  return {
    ...initialState(topic),
    subscriber: stored.subscriber,
    checkpoint: stored.checkpoint,
    events: stored.events,
    evicted: stored.evicted,
  };
};

export const persistable = (state: ConsumerState): string =>
  JSON.stringify({
    version: STORED_VERSION,
    topic: state.topic,
    subscriber: state.subscriber,
    checkpoint: state.checkpoint,
    events: state.events,
    evicted: state.evicted,
  });
