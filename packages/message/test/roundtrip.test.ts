import { Result } from "effect";
import fc from "fast-check";
import { expect, it } from "vitest";

import { type CloudEventInit, make, parse, serialize } from "../src/index.js";
import { baseInit, baseWire, initArb, wireArb } from "./support.js";

it("any accepted wire object serializes back to the same object", () => {
  fc.assert(
    fc.property(wireArb, (wire) => {
      const parsed = parse(wire);
      expect(Result.isSuccess(parsed)).toBe(true);
      if (Result.isFailure(parsed)) return;
      expect(serialize(parsed.success)).toEqual(wire);
    }),
  );
});

it("any constructed event parses back to the same event", () => {
  fc.assert(
    fc.property(initArb, (init) => {
      const event = make(init as CloudEventInit);
      const parsed = parse(serialize(event));
      expect(Result.isSuccess(parsed)).toBe(true);
      if (Result.isFailure(parsed)) return;
      expect(parsed.success).toEqual(event);
    }),
  );
});

it("a malformed wire object is rejected", () => {
  const malformed = fc.oneof(
    wireArb.map((wire) => ({ ...wire, id: "" })),
    wireArb.map((wire) => ({ ...wire, source: "" })),
    wireArb.map((wire) => ({ ...wire, type: "" })),
    wireArb.map((wire) => ({ ...wire, subject: "" })),
    wireArb.map((wire) => ({ ...wire, id: undefined })),
    wireArb.map((wire) => ({ ...wire, specversion: "0.3" })),
    wireArb.map((wire) => ({ ...wire, specversion: "" })),
    wireArb.map((wire) => ({ ...wire, "Bad-Name": "x" })),
    wireArb.map((wire) => ({ ...wire, traceParent: "x" })),
    wireArb.map((wire) => ({ ...wire, "": "x" })),
    wireArb.map((wire) => ({ ...wire, ext: 1.5 })),
    wireArb.map((wire) => ({ ...wire, ext: 2147483648 })),
    wireArb.map((wire) => ({ ...wire, ext: null })),
    fc.constantFrom(null, [], 1, "event", undefined, true),
  );
  fc.assert(
    fc.property(malformed, (input) => {
      expect(Result.isFailure(parse(input))).toBe(true);
    }),
  );
});

it("the constructed event carries the injected specversion", () => {
  const event = make(baseInit);
  expect(event.specversion).toBe("1.0");
  expect(serialize(event)).toEqual(baseWire);
});
