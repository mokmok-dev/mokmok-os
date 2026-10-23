import { Result } from "effect";
import { expect, it } from "vitest";

import { type CloudEventInit, make, parse, serialize } from "../src/index.js";
import { baseInit, baseWire, expectMakeFailure, expectParseFailure } from "./support.js";

const expectAccepted = (field: string, value: unknown): void => {
  const wire = { ...baseWire, [field]: value };
  expect(serialize(Result.getOrThrow(parse(wire)))).toEqual(wire);
  expect(serialize(make({ ...baseInit, [field]: value } as CloudEventInit))).toEqual(wire);
};

const expectRejected = (field: string, value: unknown): void => {
  expectParseFailure({ ...baseWire, [field]: value });
  expectMakeFailure({ ...baseInit, [field]: value });
};

it("id: accepts a non-empty value", () => {
  expectAccepted("id", "abc-def");
});

it("id: rejects an empty value", () => {
  expectRejected("id", "");
});

it("type: accepts a non-empty value", () => {
  expectAccepted("type", "com.example.order.created");
});

it("type: rejects an empty value", () => {
  expectRejected("type", "");
});

it("subject: accepts a non-empty value", () => {
  expectAccepted("subject", "order-1");
});

it("subject: rejects an empty value", () => {
  expectRejected("subject", "");
});

it("source: accepts a relative URI reference", () => {
  expectAccepted("source", "/mokmok/tui");
});

it("source: accepts an absolute URI reference", () => {
  expectAccepted("source", "https://example.com/x");
});

it("source: rejects an empty value", () => {
  expectRejected("source", "");
});

it("source: rejects a value outside the URI reference character set", () => {
  expectRejected("source", "a b");
  expectRejected("source", " ");
});

it("datacontenttype: accepts a media type without parameters", () => {
  expectAccepted("datacontenttype", "application/json");
});

it("datacontenttype: accepts a media type with parameters", () => {
  expectAccepted("datacontenttype", "text/xml; charset=utf-8");
  expectAccepted("datacontenttype", "application/json;charset=utf-8");
  expectAccepted("datacontenttype", "application/json; charset=utf-8");
  expectAccepted("datacontenttype", "application/json ;charset=utf-8");
  expectAccepted("datacontenttype", 'application/json;charset=""');
  expectAccepted("datacontenttype", 'application/json;charset="abc"');
});

it("datacontenttype: rejects a value that is not a media type", () => {
  expectRejected("datacontenttype", "json");
  expectRejected("datacontenttype", " application/json");
  expectRejected("datacontenttype", "application/json ");
  expectRejected("datacontenttype", " /json");
  expectRejected("datacontenttype", "application/ ");
  expectRejected("datacontenttype", "application/json;charset=");
  expectRejected("datacontenttype", "application/json;=x");
  expectRejected("datacontenttype", "application/json; @=x");
  expectRejected("datacontenttype", "application/json;charset=@");
});

it("dataschema: accepts an absolute URI", () => {
  expectAccepted("dataschema", "https://example.com/x");
  expectAccepted("dataschema", "urn:example:1");
});

it("dataschema: rejects a URI reference without a scheme", () => {
  expectRejected("dataschema", "example.com/x");
});

it("dataschema: rejects a malformed absolute URI", () => {
  expectRejected("dataschema", " x:y");
  expectRejected("dataschema", "x:y ");
  expectRejected("dataschema", "1x:y");
  expectRejected("dataschema", "x_:y");
  expectRejected("dataschema", "x:");
  expectRejected("dataschema", "x: ");
});

it("time: accepts an RFC 3339 timestamp with a zone", () => {
  expectAccepted("time", "2018-04-05T17:31:00Z");
  expectAccepted("time", "2018-04-05T17:31:00+09:00");
  expectAccepted("time", "2018-04-05T17:31:00.12Z");
  expectAccepted("time", "2018-04-05t17:31:60Z");
  expectAccepted("time", "2026-02-30T00:00:00Z");
});

it("time: rejects a timestamp without a zone", () => {
  expectRejected("time", "2018-04-05T17:31:00");
});

it("time: rejects a timestamp with an out-of-range field", () => {
  expectRejected("time", " 2018-04-05T17:31:00Z");
  expectRejected("time", "2018-04-05T17:31:00Z ");
  expectRejected("time", "2-04-05T17:31:00Z");
  expectRejected("time", "abcd-04-05T17:31:00Z");
  expectRejected("time", "2018-00-05T17:31:00Z");
  expectRejected("time", "2018-13-05T17:31:00Z");
  expectRejected("time", "2018-04-00T17:31:00Z");
  expectRejected("time", "2018-04-39T17:31:00Z");
  expectRejected("time", "2018-04-1aT17:31:00Z");
  expectRejected("time", "2018-04-32T17:31:00Z");
  expectRejected("time", "2018-04-05X17:31:00Z");
  expectRejected("time", "2018-04-05T44:31:00Z");
  expectRejected("time", "2018-04-05T1a:31:00Z");
  expectRejected("time", "2018-04-05T24:31:00Z");
  expectRejected("time", "2018-04-05T17:61:00Z");
  expectRejected("time", "2018-04-05T17:3a:00Z");
  expectRejected("time", "2018-04-05T17:31:71Z");
  expectRejected("time", "2018-04-05T17:31:0aZ");
  expectRejected("time", "2018-04-05T17:31:00.aZ");
  expectRejected("time", "2018-04-05T17:31:00X17:31");
  expectRejected("time", "2018-04-05T17:31:00+44:00");
  expectRejected("time", "2018-04-05T17:31:00+1a:00");
  expectRejected("time", "2018-04-05T17:31:00+24:00");
  expectRejected("time", "2018-04-05T17:31:00+05:61");
  expectRejected("time", "2018-04-05T17:31:00+05:1a");
});

it("data: accepts any JSON value", () => {
  expectAccepted("data", null);
  expectAccepted("data", { sessionId: "s", tags: [1, true, null] });
  expectAccepted("data", []);
  expectAccepted("data", "text");
  expectAccepted("data", 0);
  expectAccepted("data", false);
});

it("optional: an absent attribute stays absent on the wire", () => {
  const event = Result.getOrThrow(parse(baseWire));
  expect("subject" in event).toBe(false);
  expect("subject" in serialize(event)).toBe(false);
});

it("optional: an explicit undefined value is rejected", () => {
  expectParseFailure({ ...baseWire, subject: undefined });
  expectMakeFailure({ ...baseInit, subject: undefined });
});
