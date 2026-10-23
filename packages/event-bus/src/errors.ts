import { Data } from "effect";

export class LogFailure extends Data.TaggedError("LogFailure")<{
  readonly cause: unknown;
}> {}

export class LogCorrupt extends Data.TaggedError("LogCorrupt")<{
  readonly cause: unknown;
}> {}

export class PublishFailed extends Data.TaggedError("PublishFailed")<{
  readonly cause: unknown;
}> {}

export class LogReadFailed extends Data.TaggedError("LogReadFailed")<{
  readonly cause: unknown;
}> {}

export class SubscriberLagged extends Data.TaggedError("SubscriberLagged")<{}> {}

export class EventBusStartFailed extends Data.TaggedError("EventBusStartFailed")<{
  readonly cause: unknown;
}> {}

export type SubscribeError = LogReadFailed | SubscriberLagged;
