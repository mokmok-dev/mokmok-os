import { expect, it } from "vitest";

import { make, serialize } from "../src/index.js";
import { baseInit, baseWire, CORE_NAMES, expectMakeFailure } from "./support.js";

it("make supplies the specversion", () => {
  const event = make(baseInit);
  expect(event.specversion).toBe("1.0");
  expect(serialize(event).specversion).toBe("1.0");
});

it("make flattens extensions onto the wire", () => {
  const event = make({
    ...baseInit,
    extensions: { traceparent: "00-abc-def-01", attempt: 2 },
  });
  expect(event.extensions).toEqual({ traceparent: "00-abc-def-01", attempt: 2 });
  expect(serialize(event)).toEqual({
    ...baseWire,
    traceparent: "00-abc-def-01",
    attempt: 2,
  });
});

it("make refuses a reserved extension name rather than overwriting a core attribute", () => {
  for (const name of CORE_NAMES) {
    expectMakeFailure({ ...baseInit, extensions: { [name]: "clobbered" } });
  }
});

it("make refuses a malformed extension name rather than dropping it", () => {
  expectMakeFailure({ ...baseInit, extensions: { traceParent: "x" } });
  expectMakeFailure({ ...baseInit, extensions: { "bad-name": "x" } });
});

it("make invents no optional attribute", () => {
  expect(serialize(make(baseInit))).toEqual(baseWire);
});

it("make validates exactly like parse", () => {
  expectMakeFailure({ ...baseInit, id: "" });
  expectMakeFailure({ ...baseInit, source: "a b" });
  expectMakeFailure({ ...baseInit, time: "2018-04-05T17:31:00" });
  expectMakeFailure({ ...baseInit, data: new Date() } as never);
});
