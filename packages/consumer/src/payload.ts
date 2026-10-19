import { Schema } from "effect";

export const MAX_PAYLOAD_BYTES = 262_144;

const PREVIEW_LIMIT = 80;
const ELLIPSIS = "…";

const encoder = new TextEncoder();
const JsonSchema = Schema.Json;

type Json = Schema.Schema.Type<typeof JsonSchema>;

export type ParsedPayload =
  | { readonly ok: true; readonly payload: Json }
  | { readonly ok: false; readonly message: string };

const EMPTY_MESSAGE = "the payload is empty";
const INVALID_MESSAGE = "the payload is not valid JSON";
const TOO_LARGE_MESSAGE = `the payload is larger than ${MAX_PAYLOAD_BYTES} bytes`;

export const payloadBytes = (payload: Json): number =>
  encoder.encode(JSON.stringify(payload)).length;

const accept = (text: string): ParsedPayload => {
  const decoded = Schema.decodeUnknownOption(JsonSchema)(JSON.parse(text));

  if (decoded._tag !== "Some") {
    return { ok: false, message: INVALID_MESSAGE };
  }

  if (payloadBytes(decoded.value) > MAX_PAYLOAD_BYTES) {
    return { ok: false, message: TOO_LARGE_MESSAGE };
  }

  return { ok: true, payload: decoded.value };
};

export const parsePayload = (text: string): ParsedPayload => {
  const trimmed = text.trim();

  if (trimmed.length === 0) {
    return { ok: false, message: EMPTY_MESSAGE };
  }

  try {
    return accept(trimmed);
  } catch {
    return { ok: false, message: INVALID_MESSAGE };
  }
};

export const payloadPreview = (
  payload: Json,
  limit: number = PREVIEW_LIMIT,
): string => {
  const text = JSON.stringify(payload);

  if (text.length > limit) {
    return `${text.slice(0, limit)}${ELLIPSIS}`;
  }

  return text;
};

export const formatBytes = (bytes: number): string => {
  if (bytes < 1024) {
    return `${bytes} B`;
  }

  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`;
  }

  if (bytes < 1024 * 1024 * 1024) {
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
};

const SECOND_MS = 1000;
const MINUTE_MS = 60 * SECOND_MS;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

export const relativeTime = (nowMs: number, thenMs: number): string => {
  const elapsed = nowMs - thenMs;

  if (elapsed < 0) {
    return "in the future";
  }

  if (elapsed < SECOND_MS) {
    return "just now";
  }

  if (elapsed < MINUTE_MS) {
    return `${Math.floor(elapsed / SECOND_MS)}s ago`;
  }

  if (elapsed < HOUR_MS) {
    return `${Math.floor(elapsed / MINUTE_MS)}m ago`;
  }

  if (elapsed < DAY_MS) {
    return `${Math.floor(elapsed / HOUR_MS)}h ago`;
  }

  return `${Math.floor(elapsed / DAY_MS)}d ago`;
};
