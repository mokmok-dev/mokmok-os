import { Option, Schema } from "effect";
import { LogCorrupt } from "./errors.ts";
import { parseJson, stringifyJson } from "./json.ts";

export type Payload = typeof Schema.Json.Type;

export interface Event {
  readonly seq: number;
  readonly topic: string;
  readonly payload: Payload;
}

export interface ParsedLog {
  readonly events: ReadonlyArray<Event>;
  readonly validBytes: number;
}

const EventLine = Schema.Struct({
  seq: Schema.Natural,
  topic: Schema.String,
  payload: Schema.Json,
});

const encoder = new TextEncoder();
const decoder = new TextDecoder();

const parseLine = (text: string): Event | undefined =>
  Option.getOrUndefined(Option.flatMap(parseJson(text), Schema.decodeUnknownOption(EventLine)));

export const encodeEventLine = (event: Event): Uint8Array =>
  encoder.encode(stringifyJson(event) + "\n");

export const parseLog = (bytes: Uint8Array): ParsedLog => {
  const events: Array<Event> = [];
  let validBytes = 0;
  let start = 0;
  for (const [end, byte] of bytes.entries()) {
    if (byte !== 0x0a) continue;
    const text = decoder.decode(bytes.subarray(start, end));
    const event = parseLine(text);
    if (event === undefined) {
      throw new LogCorrupt({ cause: text });
    }
    if (event.seq !== events.length + 1) {
      throw new LogCorrupt({ cause: event });
    }
    events.push(event);
    validBytes = end + 1;
    start = end + 1;
  }
  return { events, validBytes };
};
