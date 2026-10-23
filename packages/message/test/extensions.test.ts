import { Result } from "effect";
import { expect, it } from "vitest";

import { type CloudEvent, make, parse, serialize } from "../src/index.js";
import {
  baseInit,
  baseWire,
  CORE_NAMES,
  expectMakeFailure,
  expectParseFailure,
  expectSerializeFailure,
} from "./support.js";

const extensionsOf = (extensions: Record<string, boolean | number | string>) =>
  make({ ...baseInit, extensions }).extensions;

it("extension values: every kind stays on the wire", () => {
  const wire = {
    ...baseWire,
    flag: true,
    off: false,
    count: 7,
    negative: -2147483648,
    large: 2147483647,
    label: "text",
  };
  const event = Result.getOrThrow(parse(wire));
  expect(event.extensions).toEqual({
    flag: true,
    off: false,
    count: 7,
    negative: -2147483648,
    large: 2147483647,
    label: "text",
  });
  expect(serialize(event)).toEqual(wire);
});

it("extension values: the int32 bounds are accepted and one past them is rejected", () => {
  expect(extensionsOf({ min: -2147483648 })).toEqual({ min: -2147483648 });
  expect(extensionsOf({ max: 2147483647 })).toEqual({ max: 2147483647 });

  expectMakeFailure({ ...baseInit, extensions: { low: -2147483649 } });
  expectParseFailure({ ...baseWire, low: -2147483649 });
  expectMakeFailure({ ...baseInit, extensions: { high: 2147483648 } });
  expectParseFailure({ ...baseWire, high: 2147483648 });
});

it("extension values: a number that is not int32 is rejected", () => {
  expectMakeFailure({ ...baseInit, extensions: { fraction: 1.5 } });
  expectParseFailure({ ...baseWire, fraction: 1.5 });
});

it("extension values: a value that is not boolean, int32, or string is rejected", () => {
  expectMakeFailure({ ...baseInit, extensions: { value: null } as never });
  expectMakeFailure({ ...baseInit, extensions: { value: { a: 1 } } as never });
  expectMakeFailure({ ...baseInit, extensions: { value: [1] } as never });
  expectParseFailure({ ...baseWire, value: null });
  expectParseFailure({ ...baseWire, value: { a: 1 } });
});

it("extension names: lowercase alphanumeric names are accepted", () => {
  expect(extensionsOf({ traceparent: "00-abc-def-01" })).toEqual({
    traceparent: "00-abc-def-01",
  });
  expect(extensionsOf({ a: 1, abc123: true })).toEqual({ a: 1, abc123: true });
});

it("extension names: a name outside the lowercase alphanumeric grammar is rejected", () => {
  for (const name of ["", "a-b", "a_b", "traceParent", "ID", "Bad-Name"]) {
    expectMakeFailure({ ...baseInit, extensions: { [name]: "x" } });
    expectParseFailure({ ...baseWire, [name]: "x" });
  }
});

it("reserved names: a core attribute name is refused as an extension", () => {
  for (const name of CORE_NAMES) {
    expectMakeFailure({ ...baseInit, extensions: { [name]: "x" } });
  }
});

it("reserved names: a forged reserved extension is refused on serialize", () => {
  const event = make(baseInit);
  expectSerializeFailure({ ...event, extensions: { data: "x" } } as CloudEvent);
  expectSerializeFailure({ ...event, extensions: { id: "2" } } as CloudEvent);
});

it("reserved names: a forged extension value is refused on serialize", () => {
  const event = make(baseInit);
  expectSerializeFailure({ ...event, extensions: { ext: 1.5 } } as CloudEvent);
});

it("reserved names: an object payload is parsed as data instead of an extension", () => {
  const wire = { ...baseWire, data: { nested: ["value"] } };
  const event = Result.getOrThrow(parse(wire));
  expect(event.data).toEqual({ nested: ["value"] });
  expect(event.extensions).toEqual({});
});

it("presence: the event always carries an extensions record", () => {
  expect(make(baseInit).extensions).toEqual({});
  expect(Result.getOrThrow(parse(baseWire)).extensions).toEqual({});
});

it("presence: extensions are flattened onto the wire and the record is dropped", () => {
  const event = make({ ...baseInit, extensions: { traceparent: "abc" } });
  const serialized = serialize(event);
  expect(serialized).toEqual({ ...baseWire, traceparent: "abc" });
  expect("extensions" in serialized).toBe(false);
});
