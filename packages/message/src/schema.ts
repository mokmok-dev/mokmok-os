import type { Brand } from "effect";
import { Schema } from "effect";

export const SPECVERSION = "1.0";

// Schema.brand stores its label as a string literal that no runtime value
// reads, so Stryker can never kill it. Schema.Opaque carries the same brand in
// the type only, with no literal to survive.
export type Id = string & Brand.Brand<"Id">;
export type Source = string & Brand.Brand<"Source">;
export type EventType = string & Brand.Brand<"EventType">;
export type DataContentType = string & Brand.Brand<"DataContentType">;
export type DataSchema = string & Brand.Brand<"DataSchema">;
export type Subject = string & Brand.Brand<"Subject">;
export type Time = string & Brand.Brand<"Time">;
export type ExtensionName = string & Brand.Brand<"ExtensionName">;
export type Integer32 = number & Brand.Brand<"Integer32">;
export type ExtensionValue = boolean | Integer32 | string;

const SOURCE_PATTERN = /^[A-Za-z0-9\-._~:/?#[\]@!$&'()*+,;=%]+$/;
const MEDIA_TYPE_PATTERN =
  /^[A-Za-z0-9!#$%&'*+.^_`|~-]+\/[A-Za-z0-9!#$%&'*+.^_`|~-]+(?:\s*;\s*[A-Za-z0-9!#$%&'*+.^_`|~-]+=(?:[A-Za-z0-9!#$%&'*+.^_`|~-]+|"(?:[^"\\]|\\.)*"))*$/;
const DATA_SCHEMA_PATTERN = /^[A-Za-z][A-Za-z0-9+.-]*:[A-Za-z0-9\-._~:/?#[\]@!$&'()*+,;=%]+$/;
const TIME_PATTERN =
  /^\d{4}-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12]\d|3[01])[Tt](?:[01]\d|2[0-3]):[0-5]\d:(?:[0-5]\d|60)(?:\.\d+)?(?:Z|[+-](?:[01]\d|2[0-3]):[0-5]\d)$/;
const EXTENSION_NAME_PATTERN = /^[a-z0-9]+$/;

const IdSchema = Schema.NonEmptyString.pipe(Schema.Opaque<Id>());
const SourceSchema = Schema.String.pipe(
  Schema.check(Schema.isPattern(SOURCE_PATTERN)),
  Schema.Opaque<Source>(),
);
const EventTypeSchema = Schema.NonEmptyString.pipe(Schema.Opaque<EventType>());
const DataContentTypeSchema = Schema.String.pipe(
  Schema.check(Schema.isPattern(MEDIA_TYPE_PATTERN)),
  Schema.Opaque<DataContentType>(),
);
const DataSchemaSchema = Schema.String.pipe(
  Schema.check(Schema.isPattern(DATA_SCHEMA_PATTERN)),
  Schema.Opaque<DataSchema>(),
);
const SubjectSchema = Schema.NonEmptyString.pipe(Schema.Opaque<Subject>());
const TimeSchema = Schema.String.pipe(
  Schema.check(Schema.isPattern(TIME_PATTERN)),
  Schema.Opaque<Time>(),
);
// The bounds live inside the predicate, not in an isBetween options object:
// an object literal is read once at import, and Stryker activates runtime
// mutants only after import, so the bound decision would never be observed.
const Integer32Schema = Schema.Int.pipe(
  Schema.check(Schema.makeFilter((value: number) => value >= -2147483648 && value <= 2147483647)),
  Schema.Opaque<Integer32>(),
);
const ExtensionValueSchema = Schema.Union([Schema.Boolean, Integer32Schema, Schema.String]);

const AttributeFields = {
  id: IdSchema,
  source: SourceSchema,
  type: EventTypeSchema,
  datacontenttype: Schema.optionalKey(DataContentTypeSchema),
  dataschema: Schema.optionalKey(DataSchemaSchema),
  subject: Schema.optionalKey(SubjectSchema),
  time: Schema.optionalKey(TimeSchema),
  data: Schema.optionalKey(Schema.Json),
};

export const Core = Schema.Struct({
  specversion: Schema.Literal(SPECVERSION),
  ...AttributeFields,
});

// Extension names exclude the core attributes so the rest record in Wire can
// never select a core member and re-validate `data` through ExtensionValue,
// which would reject every object payload.
const CORE_NAMES: ReadonlySet<string> = new Set(Object.keys(Core.fields));

const ExtensionNameSchema = Schema.String.pipe(
  Schema.check(Schema.isPattern(EXTENSION_NAME_PATTERN)),
  Schema.check(Schema.makeFilter((name: string) => !CORE_NAMES.has(name))),
  Schema.Opaque<ExtensionName>(),
);

export const ExtensionAttributes = Schema.Record(ExtensionNameSchema, ExtensionValueSchema);

export const Init = Schema.Struct({
  ...AttributeFields,
  extensions: Schema.optionalKey(ExtensionAttributes),
});

const REST = [ExtensionAttributes];

export const Wire = Schema.StructWithRest(Core, REST);
export const ClosedExtensions = Schema.StructWithRest(Schema.Struct({}), REST);
