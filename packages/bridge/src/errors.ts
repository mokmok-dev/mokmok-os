import { Data } from "effect";

export class BusTransportError extends Data.TaggedError("BusTransportError")<{
  readonly detail: string;
}> {}

export class BridgeStartError extends Data.TaggedError("BridgeStartError")<{
  readonly detail: string;
}> {}

export class BrowserSendError extends Data.TaggedError("BrowserSendError")<{
  readonly detail: string;
}> {}

export class PublishError extends Data.TaggedError("PublishError")<{
  readonly detail: string;
  readonly indeterminate: boolean;
}> {}
