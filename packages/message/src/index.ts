import { Result, Schema } from "effect";
import { ClosedExtensions, Core, ExtensionAttributes, Init, SPECVERSION, Wire } from "./schema.js";
import type { ExtensionName, ExtensionValue } from "./schema.js";

export type {
  DataContentType,
  DataSchema,
  EventType,
  ExtensionName,
  ExtensionValue,
  Id,
  Integer32,
  Source,
  Subject,
  Time,
} from "./schema.js";

export type JsonValue = Schema.Json;

export type CloudEvent = (typeof Core)["Type"] & {
  readonly extensions: Readonly<Record<ExtensionName, ExtensionValue>>;
};

export type CloudEventInit = (typeof Init)["Encoded"];

export type CloudEventJson = (typeof Wire)["Encoded"];

const readWire = Schema.decodeUnknownResult(Wire, { onExcessProperty: "error" });
const readInit = Schema.decodeUnknownResult(Init, { onExcessProperty: "error" });
const writeExtensions = Schema.encodeResult(ClosedExtensions, { onExcessProperty: "error" });

export function parse(input: unknown): Result.Result<CloudEvent, Schema.SchemaError> {
  // The Wire decode places every own member, so each projection selects from an
  // object already proven complete and cannot fail.
  return Result.map(readWire(input), (flat) => ({
    ...Schema.decodeUnknownSync(Core)(flat),
    extensions: Schema.decodeUnknownSync(ExtensionAttributes)(flat),
  }));
}

export function make(init: CloudEventInit): CloudEvent {
  const { extensions, ...core } = Result.getOrThrow(readInit(init));
  return Result.getOrThrow(parse({ specversion: SPECVERSION, ...core, ...extensions }));
}

export function serialize(event: CloudEvent): CloudEventJson {
  return Result.match(writeExtensions(event.extensions), {
    onFailure: (cause) => {
      throw cause;
    },
    onSuccess: (extensions) => ({ ...Schema.encodeSync(Core)(event), ...extensions }),
  });
}
