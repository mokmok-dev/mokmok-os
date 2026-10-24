import { Option, Schema } from "effect";
import type { Payload } from "./event.ts";
import { parseJson, stringifyJson } from "./json.ts";

export type ClientFrame =
  | {
      readonly op: "publish";
      readonly req: number;
      readonly topic: string;
      readonly payload: Payload;
    }
  | {
      readonly op: "subscribe";
      readonly req: number;
      readonly topic: string;
      readonly after: number;
    };

export type ServerFrame =
  | { readonly op: "hello"; readonly head: number }
  | { readonly op: "published"; readonly req: number; readonly seq: number }
  | {
      readonly op: "event";
      readonly seq: number;
      readonly topic: string;
      readonly payload: Payload;
    }
  | { readonly op: "error"; readonly req: number; readonly code: "write_failed" };

const PublishFrame = Schema.Struct({
  op: Schema.Literal("publish"),
  req: Schema.Natural,
  topic: Schema.NonEmptyString,
  payload: Schema.Json,
});

const SubscribeFrame = Schema.Struct({
  op: Schema.Literal("subscribe"),
  req: Schema.Natural,
  topic: Schema.NonEmptyString,
  after: Schema.Natural,
});

const ClientFrameSchema = Schema.Union([PublishFrame, SubscribeFrame]);

export const decodeClientFrame = (text: string): ClientFrame | undefined =>
  Option.getOrUndefined(
    Option.flatMap(parseJson(text), Schema.decodeUnknownOption(ClientFrameSchema)),
  );

export const encodeServerFrame: (frame: ServerFrame) => string = stringifyJson;
