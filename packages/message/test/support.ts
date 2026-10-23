import { Result, Schema } from "effect";
import fc from "fast-check";
import { expect } from "vitest";

import { type CloudEvent, type CloudEventInit, make, parse, serialize } from "../src/index.js";

export const CORE_NAMES = [
  "specversion",
  "id",
  "source",
  "type",
  "datacontenttype",
  "dataschema",
  "subject",
  "time",
  "data",
];

export const baseWire = {
  specversion: "1.0",
  id: "1",
  source: "/mokmok/tui",
  type: "com.mokmok.session.started",
};

export const baseInit = {
  id: "1",
  source: "/mokmok/tui",
  type: "com.mokmok.session.started",
};

export function expectParseFailure(value: unknown): void {
  const result = parse(value);
  expect(Result.isFailure(result)).toBe(true);
  if (Result.isFailure(result)) expect(Schema.isSchemaError(result.failure)).toBe(true);
}

export function expectMakeFailure(init: unknown): void {
  expect(() => make(init as CloudEventInit)).toThrow(Schema.SchemaError);
}

export function expectSerializeFailure(event: unknown): void {
  expect(() => serialize(event as CloudEvent)).toThrow(Schema.SchemaError);
}

const pad = (n: number) => String(n).padStart(2, "0");

const uriCharacters = /^[A-Za-z0-9\-._~:/?#[\]@!$&'()*+,;=%]+$/;

export const sourceArb = fc.stringMatching(uriCharacters);

export const dataSchemaArb = fc
  .tuple(fc.stringMatching(/^[A-Za-z][A-Za-z0-9+.-]*$/), fc.stringMatching(uriCharacters))
  .map(([scheme, rest]) => `${scheme}:${rest}`);

const tokenArb = fc.stringMatching(/^[A-Za-z0-9!#$%&'*+.^_`|~-]+$/);

export const mediaTypeArb = fc
  .tuple(tokenArb, tokenArb)
  .map(([kind, subtype]) => `${kind}/${subtype}`);

export const timeArb = fc
  .tuple(
    fc.integer({ min: 0, max: 9999 }).map((n) => String(n).padStart(4, "0")),
    fc.integer({ min: 1, max: 12 }),
    fc.integer({ min: 1, max: 31 }),
    fc.integer({ min: 0, max: 23 }),
    fc.integer({ min: 0, max: 59 }),
    fc.integer({ min: 0, max: 60 }),
    fc.option(
      fc.integer({ min: 0, max: 999 }).map((n) => `.${String(n).padStart(3, "0")}`),
      {
        nil: undefined,
      },
    ),
    fc.oneof(
      fc.constant("Z"),
      fc
        .tuple(fc.integer({ min: 0, max: 23 }), fc.integer({ min: 0, max: 59 }))
        .map(([hours, minutes]) => `+${pad(hours)}:${pad(minutes)}`),
    ),
  )
  .map(
    ([year, month, day, hours, minutes, seconds, fraction, zone]) =>
      `${year}-${pad(month)}-${pad(day)}T${pad(hours)}:${pad(minutes)}:${pad(seconds)}${fraction ?? ""}${zone}`,
  );

const extensionNameArb = fc
  .stringMatching(/^[a-z0-9]+$/)
  .filter((name) => !CORE_NAMES.includes(name));

const extensionValueArb = fc.oneof(
  fc.boolean(),
  fc.integer({ min: -2147483648, max: 2147483647 }),
  fc.string(),
);

const extensionsArb = fc.dictionary(extensionNameArb, extensionValueArb);

export const wireArb = fc
  .tuple(
    fc.string({ minLength: 1 }),
    sourceArb,
    fc.string({ minLength: 1 }),
    fc.option(mediaTypeArb, { nil: undefined }),
    fc.option(dataSchemaArb, { nil: undefined }),
    fc.option(fc.string({ minLength: 1 }), { nil: undefined }),
    fc.option(timeArb, { nil: undefined }),
    fc.option(fc.jsonValue(), { nil: undefined }),
    extensionsArb,
  )
  .map(([id, source, type, datacontenttype, dataschema, subject, time, data, extensions]) =>
    Object.fromEntries([
      ["specversion", "1.0"],
      ["id", id],
      ["source", source],
      ["type", type],
      ...(datacontenttype === undefined ? [] : [["datacontenttype", datacontenttype]]),
      ...(dataschema === undefined ? [] : [["dataschema", dataschema]]),
      ...(subject === undefined ? [] : [["subject", subject]]),
      ...(time === undefined ? [] : [["time", time]]),
      ...(data === undefined ? [] : [["data", data]]),
      ...Object.entries(extensions),
    ]),
  );

export const initArb = fc
  .tuple(
    fc.string({ minLength: 1 }),
    sourceArb,
    fc.string({ minLength: 1 }),
    fc.option(mediaTypeArb, { nil: undefined }),
    fc.option(dataSchemaArb, { nil: undefined }),
    fc.option(fc.string({ minLength: 1 }), { nil: undefined }),
    fc.option(timeArb, { nil: undefined }),
    fc.option(fc.jsonValue(), { nil: undefined }),
    fc.option(extensionsArb, { nil: undefined }),
  )
  .map(([id, source, type, datacontenttype, dataschema, subject, time, data, extensions]) => ({
    id,
    source,
    type,
    ...(datacontenttype === undefined ? {} : { datacontenttype }),
    ...(dataschema === undefined ? {} : { dataschema }),
    ...(subject === undefined ? {} : { subject }),
    ...(time === undefined ? {} : { time }),
    ...(data === undefined ? {} : { data }),
    ...(extensions === undefined ? {} : { extensions }),
  }));
