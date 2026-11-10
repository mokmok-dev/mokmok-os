import { Effect } from 'effect';
import { afterEach, beforeEach, test, expect, vi } from 'vitest';

import {
  ApiError,
  CONNECTION_KEY,
  INVALID_JSON,
  JOURNAL_LIMIT,
  TOKEN_KEY,
  createClient,
  decodeConnection,
  normalizeBaseUrl,
  parseJson,
  readConnection,
  readToken,
  validateBaseUrl,
  writeBaseUrl,
  writeToken,
} from '../api/client';
import { message } from './fixtures';

interface Call {
  url: string;
  init: RequestInit;
}

function answer(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

function stubFetch(reply: (call: Call) => Response | Promise<Response>): {
  fetchImpl: typeof fetch;
  calls: Call[];
} {
  const calls: Call[] = [];
  const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const call = { url: String(input), init: init ?? {} };
    calls.push(call);
    return reply(call);
  }) as typeof fetch;
  return { fetchImpl, calls };
}

function headersOf(call: Call): Record<string, string> {
  return call.init.headers as Record<string, string>;
}

beforeEach(() => {
  window.localStorage.clear();
  window.sessionStorage.clear();
});

afterEach(() => {
  vi.unstubAllEnvs();
});

test('client: validates an origin-only base URL', () => {
  expect(validateBaseUrl('')).toBeNull();
  expect(validateBaseUrl('   ')).toBeNull();
  expect(validateBaseUrl('http://127.0.0.1:8787')).toBeNull();
  expect(validateBaseUrl('https://control.example')).toBeNull();
  expect(validateBaseUrl('http://host/')).toBeNull();
  expect(validateBaseUrl('http:')).not.toBeNull();
  expect(validateBaseUrl('ftp://host')).toContain('http or https');
  expect(validateBaseUrl('http://host/api')).toContain('origin');
  expect(validateBaseUrl('http://host/api/')).toContain('origin');
  expect(validateBaseUrl('http://host?x=1')).toContain('origin');
  expect(validateBaseUrl('http://host/#x')).toContain('origin');
  expect(validateBaseUrl('http://user@host')).toContain('origin');
  expect(validateBaseUrl('http://:pass@host')).toContain('origin');
  expect(validateBaseUrl('http://user:pass@host')).toContain('origin');
});

test('client: normalizes a base URL to its origin', () => {
  expect(normalizeBaseUrl('  http://127.0.0.1:8787/  ')).toBe(
    'http://127.0.0.1:8787',
  );
  expect(normalizeBaseUrl('https://host')).toBe('https://host');
  expect(normalizeBaseUrl('')).toBe('');
  expect(normalizeBaseUrl('   ')).toBe('');
  expect(() => normalizeBaseUrl('http://host/api')).toThrow();
  expect(() => normalizeBaseUrl('http:')).toThrow();
});

test('client: reads an absent base URL as empty', () => {
  expect(readConnection(window.localStorage).baseUrl).toBe('');
});

test('client: reports no warning for an absent configuration', () => {
  expect(readConnection(window.localStorage)).toEqual({
    baseUrl: '',
    warning: null,
  });
});

test('client: reports an invalid stored URL as a configuration warning', () => {
  window.localStorage.setItem(
    CONNECTION_KEY,
    JSON.stringify({ baseUrl: 'http://host/api' }),
  );
  const read = readConnection(window.localStorage);
  expect(read.baseUrl).toBe('');
  expect(read.warning).toContain('invalid');
});

test('client: reports an invalid build-time URL as a configuration warning', () => {
  vi.stubEnv('VITE_CONTROL_PLANE_URL', 'http://host/api');
  const read = readConnection(window.localStorage);
  expect(read.baseUrl).toBe('');
  expect(read.warning).toContain('invalid');
});

test('client: reads and normalizes a stored base URL', () => {
  window.localStorage.setItem(
    CONNECTION_KEY,
    JSON.stringify({ baseUrl: 'http://host:9000/' }),
  );
  expect(readConnection(window.localStorage).baseUrl).toBe('http://host:9000');
});

test('client: falls back to the build-time URL when nothing is stored', () => {
  vi.stubEnv('VITE_CONTROL_PLANE_URL', 'http://env:9000/');
  expect(readConnection(window.localStorage).baseUrl).toBe('http://env:9000');
});

test('client: a stored base URL overrides the build-time URL', () => {
  vi.stubEnv('VITE_CONTROL_PLANE_URL', 'http://env:9000');
  window.localStorage.setItem(
    CONNECTION_KEY,
    JSON.stringify({ baseUrl: 'http://stored:9001' }),
  );
  expect(readConnection(window.localStorage).baseUrl).toBe(
    'http://stored:9001',
  );
});

test('client: a stored empty base URL overrides the build-time URL', () => {
  vi.stubEnv('VITE_CONTROL_PLANE_URL', 'http://env:9000');
  window.localStorage.setItem(CONNECTION_KEY, JSON.stringify({ baseUrl: '' }));
  expect(readConnection(window.localStorage).baseUrl).toBe('');
});

test('client: an invalid build-time URL reads as empty', () => {
  vi.stubEnv('VITE_CONTROL_PLANE_URL', 'http://host/api');
  expect(readConnection(window.localStorage).baseUrl).toBe('');
});

test('client: reports a malformed stored connection and ignores the environment', () => {
  vi.stubEnv('VITE_CONTROL_PLANE_URL', 'http://env:9000');
  window.localStorage.setItem(CONNECTION_KEY, 'not json');
  const read = readConnection(window.localStorage);
  expect(read.baseUrl).toBe('');
  expect(read.warning).toContain('unreadable');
});

test('client: reports a wrongly shaped stored connection', () => {
  window.localStorage.setItem(CONNECTION_KEY, JSON.stringify({ baseUrl: 1 }));
  const read = readConnection(window.localStorage);
  expect(read.baseUrl).toBe('');
  expect(read.warning).toContain('unreadable');
});

test('client: reports a JSON-array stored connection', () => {
  window.localStorage.setItem(CONNECTION_KEY, JSON.stringify(['http://host']));
  const read = readConnection(window.localStorage);
  expect(read.baseUrl).toBe('');
  expect(read.warning).toContain('unreadable');
});

test('client: an invalid stored URL reads as empty', () => {
  window.localStorage.setItem(
    CONNECTION_KEY,
    JSON.stringify({ baseUrl: 'http://host/api' }),
  );
  expect(readConnection(window.localStorage).baseUrl).toBe('');
});

test('client: decodes a stored connection object', () => {
  expect(decodeConnection({ baseUrl: 'http://host:9000' })).toBe(
    'http://host:9000',
  );
  expect(decodeConnection({ baseUrl: 1 })).toBeNull();
  expect(decodeConnection(null)).toBeNull();
  expect(decodeConnection('nope')).toBeNull();
});

test('client: parses JSON and marks an unparseable body', () => {
  expect(parseJson('{"a":1}')).toEqual({ a: 1 });
  expect(parseJson('not json')).toBe(INVALID_JSON);
});

test('client: names the storage keys', () => {
  expect(CONNECTION_KEY).toBe('mokmok.connection.v1');
  expect(TOKEN_KEY).toBe('mokmok.control.token');
});

test('client: writes a normalized base URL', () => {
  writeBaseUrl(window.localStorage, 'http://host:9000///');
  expect(window.localStorage.getItem(CONNECTION_KEY)).toBe(
    JSON.stringify({ baseUrl: 'http://host:9000' }),
  );
});

test('client: writes a trimmed token for its origin', () => {
  writeToken(window.sessionStorage, '  secret  ', 'http://host:9000/');
  expect(
    JSON.parse(window.sessionStorage.getItem(TOKEN_KEY) ?? 'null'),
  ).toEqual({ origin: 'http://host:9000', token: 'secret' });
});

test('client: reads a token for the matching origin', () => {
  writeToken(window.sessionStorage, 'secret', 'http://host:9000');
  expect(readToken(window.sessionStorage, 'http://host:9000')).toBe('secret');
});

test('client: ignores and clears a token for another origin', () => {
  writeToken(window.sessionStorage, 'secret', 'http://host:9000');
  expect(readToken(window.sessionStorage, 'http://other:9000')).toBeNull();
  expect(window.sessionStorage.getItem(TOKEN_KEY)).toBeNull();
});

test('client: removes a legacy plain token without using it', () => {
  window.sessionStorage.setItem(TOKEN_KEY, 'legacy-secret');
  expect(readToken(window.sessionStorage, '')).toBeNull();
  expect(window.sessionStorage.getItem(TOKEN_KEY)).toBeNull();
});

test('client: removes a JSON-string token without using it', () => {
  window.sessionStorage.setItem(TOKEN_KEY, JSON.stringify('legacy-secret'));
  expect(readToken(window.sessionStorage, '')).toBeNull();
  expect(window.sessionStorage.getItem(TOKEN_KEY)).toBeNull();
});

test('client: does not touch storage when no token is stored', () => {
  const removeItem = vi.fn();
  const storage = { getItem: () => null, removeItem } as unknown as Storage;
  expect(readToken(storage, '')).toBeNull();
  expect(removeItem).not.toHaveBeenCalled();
});

test('client: rejects a token object with a non-string token', () => {
  window.sessionStorage.setItem(
    TOKEN_KEY,
    JSON.stringify({ origin: '', token: 42 }),
  );
  expect(readToken(window.sessionStorage, '')).toBeNull();
  expect(window.sessionStorage.getItem(TOKEN_KEY)).toBeNull();
});

test('client: clears a malformed token blob', () => {
  window.sessionStorage.setItem(TOKEN_KEY, '  {"broken":');
  expect(readToken(window.sessionStorage, '')).toBeNull();
  expect(window.sessionStorage.getItem(TOKEN_KEY)).toBeNull();
});

test('client: clears a JSON-array token blob', () => {
  window.sessionStorage.setItem(TOKEN_KEY, '["broken"');
  expect(readToken(window.sessionStorage, '')).toBeNull();
  expect(window.sessionStorage.getItem(TOKEN_KEY)).toBeNull();
});

test('client: clears a null token', () => {
  window.sessionStorage.setItem(TOKEN_KEY, 'secret');
  writeToken(window.sessionStorage, null, '');
  expect(window.sessionStorage.getItem(TOKEN_KEY)).toBeNull();
});

test('client: clears an empty token', () => {
  window.sessionStorage.setItem(TOKEN_KEY, 'secret');
  writeToken(window.sessionStorage, '   ', '');
  expect(window.sessionStorage.getItem(TOKEN_KEY)).toBeNull();
});

test('client: fetches the agent list', async () => {
  const { fetchImpl, calls } = stubFetch(() =>
    answer({ agents: ['scripted'], default: 'scripted' }),
  );
  const client = createClient(
    { baseUrl: 'http://cp', token: 'secret' },
    fetchImpl,
  );
  const response = await Effect.runPromise(client.fetchAgents());

  expect(response).toEqual({ agents: ['scripted'], default: 'scripted' });
  expect(calls[0]?.url).toBe('http://cp/api/agents');
  expect(calls[0]?.init.method).toBe('GET');
  expect(headersOf(calls[0] as Call)).toEqual({
    Accept: 'application/json',
    Authorization: 'Bearer secret',
  });
});

test('client: omits the authorization header without a token', async () => {
  const { fetchImpl, calls } = stubFetch(() => answer({ tasks: [] }));
  const client = createClient({ baseUrl: '', token: null }, fetchImpl);
  await Effect.runPromise(client.fetchTasks());
  expect(calls[0]?.url).toBe('/api/tasks');
  expect(calls[0]?.init.method).toBe('GET');
  expect(headersOf(calls[0] as Call).Authorization).toBeUndefined();
});

test('client: omits the authorization header for an empty token', async () => {
  const { fetchImpl, calls } = stubFetch(() => answer({ tasks: [] }));
  const client = createClient({ baseUrl: '', token: '' }, fetchImpl);
  await Effect.runPromise(client.fetchTasks());
  expect(headersOf(calls[0] as Call).Authorization).toBeUndefined();
});

test('client: posts a task and decodes its id', async () => {
  const { fetchImpl, calls } = stubFetch(() => answer({ id: 'id-1' }, 201));
  const client = createClient({ baseUrl: 'http://cp', token: null }, fetchImpl);
  const created = await Effect.runPromise(
    client.createTask({
      title: 'Fix it',
      instruction: 'Do it.',
      agent: 'scripted',
      workspace: '/work/a',
    }),
  );

  expect(created).toEqual({ id: 'id-1' });
  expect(calls[0]?.url).toBe('http://cp/api/tasks');
  expect(calls[0]?.init.method).toBe('POST');
  expect(headersOf(calls[0] as Call)['Content-Type']).toBe('application/json');
  expect(calls[0]?.init.body).toBe(
    JSON.stringify({
      title: 'Fix it',
      instruction: 'Do it.',
      agent: 'scripted',
      workspace: '/work/a',
    }),
  );
});

test('client: fetches a task without events', async () => {
  const state = {
    spec: {
      task_id: 'id-1',
      title: 'Fix it',
      instruction: 'Do it.',
      agent: 'scripted',
    },
    status: 'completed',
    update_count: 0,
    outcome: 'done',
  };
  const { fetchImpl, calls } = stubFetch(() =>
    answer({ state, events: [], event_count: 2 }),
  );
  const client = createClient({ baseUrl: 'http://cp', token: null }, fetchImpl);
  const detail = await Effect.runPromise(client.fetchTask('id-1'));
  expect(detail.state.spec?.task_id).toBe('id-1');
  expect(calls[0]?.url).toBe('http://cp/api/tasks/id-1?events=none');
  expect(calls[0]?.init.method).toBe('GET');
});

test('client: encodes a task id in the path', async () => {
  const { fetchImpl, calls } = stubFetch(() =>
    answer({ entries: [], total: 0 }),
  );
  const client = createClient({ baseUrl: '', token: null }, fetchImpl);
  await Effect.runPromise(client.fetchJournal('a/b'));
  expect(calls[0]?.url).toBe('/api/tasks/a%2Fb/journal?limit=1000');
  expect(calls[0]?.init.method).toBe('GET');
  expect(JOURNAL_LIMIT).toBe(1000);
});

test('client: reads a journal at the requested limit', async () => {
  const { fetchImpl, calls } = stubFetch(() =>
    answer({ entries: [message(1, 'hi')], total: 1 }),
  );
  const client = createClient({ baseUrl: '', token: null }, fetchImpl);
  const journal = await Effect.runPromise(client.fetchJournal('id-1', 5));
  expect(journal.entries).toHaveLength(1);
  expect(calls[0]?.url).toBe('/api/tasks/id-1/journal?limit=5');
});

test('client: cancels a task with the default reason', async () => {
  const { fetchImpl, calls } = stubFetch(() => answer({ id: 'id-1' }));
  const client = createClient({ baseUrl: '', token: null }, fetchImpl);
  await Effect.runPromise(client.cancelTask('id-1'));
  expect(calls[0]?.url).toBe('/api/tasks/id-1/cancel');
  expect(calls[0]?.init.method).toBe('POST');
  expect(calls[0]?.init.body).toBe(
    JSON.stringify({ reason: 'cancelled from the web UI' }),
  );
});

test('client: times out a request that never answers', async () => {
  const fetchImpl = (async (_input: RequestInfo | URL, init?: RequestInit) => {
    return new Promise<Response>((_resolve, reject) => {
      const signal = init?.signal;
      signal?.addEventListener('abort', () => {
        reject(signal.reason ?? new Error('aborted'));
      });
    });
  }) as typeof fetch;
  const client = createClient({ baseUrl: '', token: null }, fetchImpl, 10);
  await expect(
    Effect.runPromise(
      client.createTask({ title: 'a', instruction: 'b', agent: 'c' }),
    ),
  ).rejects.toMatchObject({
    status: 0,
    code: 'timeout',
    message: 'the request timed out',
  });
});

test('client: surfaces a JSON error body', async () => {
  const { fetchImpl } = stubFetch(() => answer({ error: 'no such task' }, 404));
  const client = createClient({ baseUrl: '', token: null }, fetchImpl);
  await expect(
    Effect.runPromise(client.fetchTask('id-1')),
  ).rejects.toMatchObject({ status: 404, message: 'no such task' });
});

test('client: names the status when an error body is not JSON', async () => {
  const { fetchImpl } = stubFetch(() => new Response('oops', { status: 500 }));
  const client = createClient({ baseUrl: '', token: null }, fetchImpl);
  await expect(Effect.runPromise(client.fetchTasks())).rejects.toMatchObject({
    status: 500,
    message: 'the control plane answered 500',
  });
});

test('client: reports a network failure', async () => {
  const { fetchImpl } = stubFetch(() => {
    throw new Error('offline');
  });
  const client = createClient({ baseUrl: '', token: null }, fetchImpl);
  await expect(Effect.runPromise(client.fetchTasks())).rejects.toMatchObject({
    status: 0,
    message: 'offline',
  });
});

test('client: reports a non-error rejection as unreachable', async () => {
  const { fetchImpl } = stubFetch(() => {
    throw 'boom';
  });
  const client = createClient({ baseUrl: '', token: null }, fetchImpl);
  await expect(Effect.runPromise(client.fetchTasks())).rejects.toMatchObject({
    status: 0,
    message: 'the control plane could not be reached',
  });
});

test('client: reports an unreadable response body', async () => {
  const response = {
    status: 502,
    ok: false,
    text: async () => {
      throw new Error('stream closed');
    },
  } as unknown as Response;
  const { fetchImpl } = stubFetch(() => response);
  const client = createClient({ baseUrl: '', token: null }, fetchImpl);
  await expect(Effect.runPromise(client.fetchTasks())).rejects.toMatchObject({
    status: 502,
    message: 'the control plane could not be reached',
  });
});

test('client: rejects a success body that is not JSON', async () => {
  const { fetchImpl } = stubFetch(
    () => new Response('not json', { status: 200 }),
  );
  const client = createClient({ baseUrl: '', token: null }, fetchImpl);
  await expect(Effect.runPromise(client.fetchTasks())).rejects.toMatchObject({
    status: 200,
    message: 'the control plane sent an unexpected response',
  });
});

test('client: rejects a success body of the wrong shape', async () => {
  const { fetchImpl } = stubFetch(() => answer({ agents: 'nope' }));
  const client = createClient({ baseUrl: '', token: null }, fetchImpl);
  await expect(Effect.runPromise(client.fetchAgents())).rejects.toMatchObject({
    status: 200,
    message: 'the control plane sent an unexpected response',
  });
});

test('client: an ApiError is an Error with a name, status, and code', () => {
  const error = new ApiError(409, 'conflict');
  expect(error).toBeInstanceOf(Error);
  expect(error.name).toBe('ApiError');
  expect(error.status).toBe(409);
  expect(error.code).toBeNull();
  expect(error.message).toBe('conflict');
  expect(new ApiError(0, 'timed out', 'timeout').code).toBe('timeout');
});
