import { Effect, Option, Schema } from 'effect';

import {
  AgentsResponseSchema,
  CancelTaskResponseSchema,
  CreateTaskResponseSchema,
  ErrorResponseSchema,
  JournalResponseSchema,
  TaskDetailResponseSchema,
  TasksResponseSchema,
  type AgentsResponse,
  type CancelTaskResponse,
  type CreateTaskRequest,
  type CreateTaskResponse,
  type JournalResponse,
  type TaskDetailResponse,
  type TasksResponse,
} from '../domain/types';

export const CONNECTION_KEY = 'mokmok.connection.v1';
export const TOKEN_KEY = 'mokmok.control.token';
export const JOURNAL_LIMIT = 1000;
export const REQUEST_TIMEOUT_MS = 30_000;

const UNREACHABLE = 'the control plane could not be reached';
const UNEXPECTED = 'the control plane sent an unexpected response';
const ORIGIN_ONLY =
  'The URL must name only an origin, such as http://127.0.0.1:8787.';

export type ApiErrorCode = 'timeout';

export class ApiError extends Error {
  readonly status: number;
  readonly code: ApiErrorCode | null;

  constructor(
    status: number,
    message: string,
    code: ApiErrorCode | null = null,
  ) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}

export interface ClientConfig {
  baseUrl: string;
  token: string | null;
}

export interface ApiClient {
  readonly fetchAgents: () => Effect.Effect<AgentsResponse, ApiError>;
  readonly fetchTasks: () => Effect.Effect<TasksResponse, ApiError>;
  readonly fetchTask: (
    id: string,
  ) => Effect.Effect<TaskDetailResponse, ApiError>;
  readonly fetchJournal: (
    id: string,
    limit?: number,
  ) => Effect.Effect<JournalResponse, ApiError>;
  readonly createTask: (
    body: CreateTaskRequest,
  ) => Effect.Effect<CreateTaskResponse, ApiError>;
  readonly cancelTask: (
    id: string,
  ) => Effect.Effect<CancelTaskResponse, ApiError>;
}

const ConnectionSchema = Schema.Struct({ baseUrl: Schema.String });
const TokenSchema = Schema.Struct({
  origin: Schema.String,
  token: Schema.String,
});

export interface ConnectionRead {
  baseUrl: string;
  warning: string | null;
}

export function validateBaseUrl(url: string): string | null {
  const trimmed = url.trim();
  if (trimmed.length === 0) {
    return null;
  }
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    return ORIGIN_ONLY;
  }
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
    return 'The URL must use http or https.';
  }
  if (
    !/^\/+$/.test(parsed.pathname) ||
    parsed.search !== '' ||
    parsed.hash !== '' ||
    parsed.username !== '' ||
    parsed.password !== ''
  ) {
    return ORIGIN_ONLY;
  }
  return null;
}

export function normalizeBaseUrl(url: string): string {
  const problem = validateBaseUrl(url);
  if (problem !== null) {
    throw new Error(problem);
  }
  const trimmed = url.trim();
  return trimmed.length === 0 ? '' : new URL(trimmed).origin;
}

export function decodeConnection(payload: unknown): string | null {
  const decoded = Schema.decodeUnknownOption(ConnectionSchema)(payload);
  return Option.isNone(decoded) ? null : decoded.value.baseUrl;
}

export function readConnection(storage: Storage): ConnectionRead {
  const raw = storage.getItem(CONNECTION_KEY);
  if (raw !== null) {
    const stored = decodeConnection(parseJson(raw));
    if (stored === null) {
      return {
        baseUrl: '',
        warning: 'The stored connection setting is unreadable.',
      };
    }
    const problem = validateBaseUrl(stored);
    if (problem !== null) {
      return {
        baseUrl: '',
        warning: `The stored control-plane URL ${JSON.stringify(
          stored,
        )} is invalid: ${problem}`,
      };
    }
    return { baseUrl: normalizeBaseUrl(stored), warning: null };
  }
  const env = import.meta.env.VITE_CONTROL_PLANE_URL ?? '';
  const problem = validateBaseUrl(env);
  if (problem !== null) {
    return {
      baseUrl: '',
      warning: `The build-time control-plane URL ${JSON.stringify(
        env,
      )} is invalid: ${problem}`,
    };
  }
  return { baseUrl: normalizeBaseUrl(env), warning: null };
}

export function writeBaseUrl(storage: Storage, url: string): void {
  storage.setItem(
    CONNECTION_KEY,
    JSON.stringify({ baseUrl: normalizeBaseUrl(url) }),
  );
}

export function readToken(storage: Storage, baseUrl: string): string | null {
  const raw = storage.getItem(TOKEN_KEY);
  if (raw === null) {
    return null;
  }
  const decoded = Schema.decodeUnknownOption(TokenSchema)(parseJson(raw));
  if (Option.isNone(decoded) || decoded.value.origin !== baseUrl) {
    storage.removeItem(TOKEN_KEY);
    return null;
  }
  return decoded.value.token;
}

export function writeToken(
  storage: Storage,
  token: string | null,
  baseUrl: string,
): void {
  const value = token === null ? '' : token.trim();
  if (value.length === 0) {
    storage.removeItem(TOKEN_KEY);
    return;
  }
  storage.setItem(
    TOKEN_KEY,
    JSON.stringify({ origin: normalizeBaseUrl(baseUrl), token: value }),
  );
}

export const INVALID_JSON = Symbol('invalid-json');

export function parseJson(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return INVALID_JSON;
  }
}

function errorMessage(payload: unknown): string | null {
  const decoded = Schema.decodeUnknownOption(ErrorResponseSchema)(payload);
  return Option.isNone(decoded) ? null : decoded.value.error;
}

function request<S extends Schema.ConstraintDecoder<unknown>>(
  baseUrl: string,
  token: string | null,
  fetchImpl: typeof fetch,
  path: string,
  init: RequestInit,
  timeoutMs: number,
  schema: S,
): Effect.Effect<S['Type'], ApiError> {
  return Effect.gen(function* () {
    const headers: Record<string, string> = { Accept: 'application/json' };
    if (init.body !== undefined) {
      headers['Content-Type'] = 'application/json';
    }
    if (token !== null) {
      headers.Authorization = `Bearer ${token}`;
    }

    const timeout = AbortSignal.timeout(timeoutMs);
    const response = yield* Effect.tryPromise({
      try: (signal) =>
        fetchImpl(`${baseUrl}${path}`, {
          ...init,
          headers,
          signal: AbortSignal.any([signal, timeout]),
        }),
      catch: (cause) =>
        timeout.aborted
          ? new ApiError(0, 'the request timed out', 'timeout')
          : new ApiError(
              0,
              cause instanceof Error ? cause.message : UNREACHABLE,
            ),
    });
    const text = yield* Effect.tryPromise({
      try: () => response.text(),
      catch: () => new ApiError(response.status, UNREACHABLE),
    });
    const payload = yield* Effect.sync(() => parseJson(text));

    if (!response.ok) {
      const message =
        errorMessage(payload) ??
        `the control plane answered ${response.status}`;
      return yield* Effect.fail(new ApiError(response.status, message));
    }
    const decoded = Schema.decodeUnknownOption(schema)(payload);
    if (Option.isNone(decoded)) {
      return yield* Effect.fail(new ApiError(response.status, UNEXPECTED));
    }
    return decoded.value;
  });
}

export function createClient(
  config: ClientConfig,
  fetchImpl: typeof fetch = globalThis.fetch,
  timeoutMs: number = REQUEST_TIMEOUT_MS,
): ApiClient {
  const baseUrl = normalizeBaseUrl(config.baseUrl);
  const token =
    config.token === null || config.token.length === 0 ? null : config.token;
  const call = <S extends Schema.ConstraintDecoder<unknown>>(
    path: string,
    init: RequestInit,
    schema: S,
  ) => request(baseUrl, token, fetchImpl, path, init, timeoutMs, schema);

  return {
    fetchAgents: () =>
      call('/api/agents', { method: 'GET' }, AgentsResponseSchema),
    fetchTasks: () =>
      call('/api/tasks', { method: 'GET' }, TasksResponseSchema),
    fetchTask: (id) =>
      call(
        `/api/tasks/${encodeURIComponent(id)}?events=none`,
        { method: 'GET' },
        TaskDetailResponseSchema,
      ),
    fetchJournal: (id, limit = JOURNAL_LIMIT) =>
      call(
        `/api/tasks/${encodeURIComponent(id)}/journal?limit=${limit}`,
        { method: 'GET' },
        JournalResponseSchema,
      ),
    createTask: (body) =>
      call(
        '/api/tasks',
        { method: 'POST', body: JSON.stringify(body) },
        CreateTaskResponseSchema,
      ),
    cancelTask: (id) =>
      call(
        `/api/tasks/${encodeURIComponent(id)}/cancel`,
        {
          method: 'POST',
          body: JSON.stringify({ reason: 'cancelled from the web UI' }),
        },
        CancelTaskResponseSchema,
      ),
  };
}
