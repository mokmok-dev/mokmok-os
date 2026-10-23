import { Result, Schema } from "effect";
import { expect, it } from "vitest";

import { type CloudEvent, type CloudEventInit, make, parse, serialize } from "../src/index.js";
import {
  baseInit,
  baseWire,
  expectMakeFailure,
  expectParseFailure,
  expectSerializeFailure,
} from "./support.js";

it("non-object input fails as data instead of throwing", () => {
  for (const input of [null, [], 1, "event", undefined, true]) {
    const result = parse(input);
    expect(Result.isFailure(result)).toBe(true);
    if (Result.isFailure(result)) expect(Schema.isSchemaError(result.failure)).toBe(true);
  }
});

it("a member whose name is not an extension name is rejected", () => {
  expectParseFailure({ ...baseWire, "Bad-Name": "x" });
  expectParseFailure({ ...baseWire, traceParent: "x" });
  expectParseFailure({ ...baseWire, ID: "x" });
  expectParseFailure({ ...baseWire, "": "x" });
});

it("an unknown member with a valid name and a bad value is rejected", () => {
  expectParseFailure({ ...baseWire, ext: 1.5 });
  expectParseFailure({ ...baseWire, ext: null });
});

it("an unknown member of make's init is rejected instead of becoming an extension", () => {
  expectMakeFailure({ ...baseInit, "bogus-name": true });
  expectMakeFailure({ ...baseInit, bogus: 1.5 });
  expectMakeFailure({ ...baseInit, bogus: true });
});

it("an optional attribute set to undefined explicitly is rejected", () => {
  expectParseFailure({ ...baseWire, subject: undefined });
  expectParseFailure({ ...baseWire, data: undefined });
});

it("specversion must be exactly 1.0", () => {
  expectParseFailure({ ...baseWire, specversion: "0.3" });
  expectParseFailure({ ...baseWire, specversion: "1" });
  expectParseFailure({ ...baseWire, specversion: "1.0 " });
  expectParseFailure({ ...baseWire, specversion: undefined });
  const { specversion, ...withoutSpecversion } = baseWire;
  void specversion;
  expectParseFailure(withoutSpecversion);
});

it("make leaves no room for a caller-supplied specversion", () => {
  expectMakeFailure({ ...baseInit, specversion: "0.3" } as CloudEventInit);
});

it("data preserves null", () => {
  const event = Result.getOrThrow(parse({ ...baseWire, data: null }));
  expect(event.data).toBeNull();
  expect(serialize(event).data).toBeNull();
});

it("data rejects a value that is not JSON", () => {
  expectParseFailure({ ...baseWire, data: new Date() });
  expectParseFailure({ ...baseWire, data: () => 1 });
  expectParseFailure({ ...baseWire, data: Number.NaN });
  expectParseFailure({ ...baseWire, data: Number.POSITIVE_INFINITY });
  expectParseFailure({ ...baseWire, data: { nested: undefined } });
  expectMakeFailure({ ...baseInit, data: new Date() } as never);
});

it("serialize revalidates every core attribute", () => {
  const event = make(baseInit);
  const forged: ReadonlyArray<readonly [string, unknown]> = [
    ["id", ""],
    ["source", "a b"],
    ["type", ""],
    ["subject", ""],
    ["datacontenttype", "json"],
    ["dataschema", "example.com/x"],
    ["time", "2018-04-05T17:31:00"],
  ];
  for (const [field, value] of forged) {
    expectSerializeFailure({ ...event, [field]: value } as CloudEvent);
  }
});

it("serialize revalidates the attribute types", () => {
  const event = make(baseInit);
  expectSerializeFailure({ ...event, id: 1 } as never);
  expectSerializeFailure({ ...event, data: new Date() } as never);
});
