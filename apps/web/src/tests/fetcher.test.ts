import type { ChatFetcher, UIMessage } from '@tanstack/ai-client';
import { EventType, type StreamChunk } from '@tanstack/ai';
import { Effect } from 'effect';
import fc from 'fast-check';
import { test, expect, vi } from 'vitest';

import {
  ApiError,
  JOURNAL_LIMIT,
  REQUEST_TIMEOUT_MS,
  type ApiClient,
} from '../api/client';
import {
  delay,
  errorText,
  lastUserText,
  makeControlPlaneFetcher,
  timeoutMessage,
} from '../api/fetcher';
import type { JournalEntry, TaskStatus } from '../domain/types';
import { message, thought, THREAD_ID, toolCall } from './fixtures';

const INPUT = {
  messages: [
    {
      id: 'm1',
      role: 'user',
      parts: [{ type: 'text', content: 'Fix the build' }],
    },
  ] as UIMessage[],
  threadId: THREAD_ID,
  runId: 'run-1',
};

function makeClient(overrides: Partial<ApiClient> = {}): ApiClient {
  return {
    fetchAgents: () =>
      Effect.succeed({ agents: ['scripted'], default: 'scripted' }),
    fetchTasks: () => Effect.succeed({ tasks: [] }),
    fetchTask: () =>
      Effect.succeed({
        state: {
          spec: {
            task_id: 'id-1',
            title: 'Fix the build',
            instruction: 'Fix the build',
            agent: 'scripted',
          },
          status: 'completed',
          update_count: 0,
          outcome: 'done',
        },
      }),
    fetchJournal: () => Effect.succeed({ entries: [], total: 0 }),
    createTask: () => Effect.succeed({ id: 'id-1' }),
    cancelTask: () => Effect.succeed({ id: 'id-1' }),
    ...overrides,
  };
}

async function collect(
  fetcher: ChatFetcher,
  input = INPUT,
  signal = new AbortController().signal,
): Promise<StreamChunk[]> {
  vi.useFakeTimers();
  try {
    const result = await fetcher(input, { signal });
    if (result instanceof Response) {
      throw new Error('the fetcher returned a response');
    }
    const iterator = result[Symbol.asyncIterator]();
    const chunks: StreamChunk[] = [];
    for (;;) {
      const next = iterator.next();
      await vi.runAllTimersAsync();
      const step = await next;
      if (step.done) {
        return chunks;
      }
      chunks.push(step.value);
      if (chunks.length > 200) {
        throw new Error('the run never finished');
      }
    }
  } finally {
    vi.useRealTimers();
  }
}

function entryArbitrary() {
  return fc
    .record({
      seq: fc.integer({ min: 1, max: 1_000 }),
      kind: fc.constantFrom('message', 'thought', 'tool_call', 'permission'),
      text: fc.string(),
      status: fc.constantFrom('completed', 'failed', 'running'),
    })
    .map(({ seq, kind, text, status }): JournalEntry => {
      switch (kind) {
        case 'message':
          return { seq, time: '', kind: 'message', text };
        case 'thought':
          return { seq, time: '', kind: 'thought', text };
        case 'tool_call':
          return {
            seq,
            time: '',
            kind: 'tool_call',
            title: text || 'tool',
            status,
          };
        default:
          return {
            seq,
            time: '',
            kind: 'permission',
            title: text || 'permission',
            status,
          };
      }
    });
}

test('fetcher: reads the last user message text', () => {
  expect(lastUserText(INPUT.messages)).toBe('Fix the build');
});

test('fetcher: joins the text parts of the last user message', () => {
  const messages: UIMessage[] = [
    { id: 'u1', role: 'user', parts: [{ type: 'text', content: 'first' }] },
    {
      id: 'a1',
      role: 'assistant',
      parts: [{ type: 'text', content: 'reply' }],
    },
    {
      id: 'u2',
      role: 'user',
      parts: [
        { type: 'text', content: 'one' },
        { type: 'text', content: 'two' },
      ],
    },
  ];
  expect(lastUserText(messages)).toBe('one\ntwo');
});

test('fetcher: ignores non-text parts', () => {
  const messages: UIMessage[] = [
    {
      id: 'u1',
      role: 'user',
      parts: [
        { type: 'text', content: 'kept' },
        {
          type: 'image',
          source: { type: 'url', value: 'http://example.test/a.png' },
        },
      ],
    },
  ];
  expect(lastUserText(messages)).toBe('kept');
});

test('fetcher: returns an empty instruction without a user message', () => {
  expect(
    lastUserText([
      { id: 'a1', role: 'assistant', parts: [{ type: 'text', content: 'hi' }] },
    ]),
  ).toBe('');
  expect(lastUserText([])).toBe('');
});

test('fetcher: finds a user message before a trailing assistant message', () => {
  const messages: UIMessage[] = [
    { id: 'u1', role: 'user', parts: [{ type: 'text', content: 'hello' }] },
    {
      id: 'a1',
      role: 'assistant',
      parts: [{ type: 'text', content: 'reply' }],
    },
  ];
  expect(lastUserText(messages)).toBe('hello');
});

test('fetcher: names an Error message', () => {
  expect(errorText(new Error('odd'))).toBe('odd');
});

test('fetcher: names an ApiError message', () => {
  expect(errorText(new ApiError(500, 'boom'))).toBe('boom');
});

test('fetcher: names an unknown failure', () => {
  expect(errorText('nope')).toBe('the control plane request failed');
});

test('fetcher: delay resolves at once for an aborted signal', async () => {
  vi.useFakeTimers();
  try {
    const controller = new AbortController();
    controller.abort();
    let settled = false;
    void delay(10_000, controller.signal).then(() => {
      settled = true;
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(settled).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
  } finally {
    vi.useRealTimers();
  }
});

test('fetcher: delay resolves when the signal aborts', async () => {
  vi.useFakeTimers();
  try {
    const controller = new AbortController();
    const removeListener = vi.spyOn(controller.signal, 'removeEventListener');
    let settled = false;
    void delay(10_000, controller.signal).then(() => {
      settled = true;
    });
    controller.abort();
    await vi.advanceTimersByTimeAsync(0);
    expect(settled).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
    expect(removeListener).toHaveBeenCalledWith('abort', expect.any(Function));
  } finally {
    vi.useRealTimers();
  }
});

test('fetcher: delay waits for its timer while the signal is live', async () => {
  vi.useFakeTimers();
  try {
    const controller = new AbortController();
    const addListener = vi.spyOn(controller.signal, 'addEventListener');
    const removeListener = vi.spyOn(controller.signal, 'removeEventListener');
    let settled = false;
    void delay(1_000, controller.signal).then(() => {
      settled = true;
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(settled).toBe(false);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(settled).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
    expect(addListener).toHaveBeenCalledWith('abort', expect.any(Function), {
      once: true,
    });
    expect(removeListener).toHaveBeenCalledWith('abort', expect.any(Function));
  } finally {
    vi.useRealTimers();
  }
});

test('fetcher: refuses a message with no text', async () => {
  const create = vi.fn(() => Effect.succeed({ id: 'id-1' }));
  const client = makeClient({ createTask: create });
  const fetcher = makeControlPlaneFetcher({
    client,
    workspace: null,
    agent: 'scripted',
  });
  const chunks = await collect(fetcher, {
    ...INPUT,
    messages: [{ id: 'u1', role: 'user', parts: [] }],
  });
  expect(chunks).toStrictEqual([
    {
      type: EventType.RUN_ERROR,
      message: 'the message has no text',
      code: 'empty-message',
    },
  ]);
  expect(create).not.toHaveBeenCalled();
});

test('fetcher: refuses a whitespace-only message', async () => {
  const create = vi.fn(() => Effect.succeed({ id: 'id-1' }));
  const client = makeClient({ createTask: create });
  const fetcher = makeControlPlaneFetcher({
    client,
    workspace: null,
    agent: 'scripted',
  });
  const chunks = await collect(fetcher, {
    ...INPUT,
    messages: [
      { id: 'u1', role: 'user', parts: [{ type: 'text', content: '   \n  ' }] },
    ],
  });
  expect(chunks[0]).toMatchObject({ code: 'empty-message' });
  expect(create).not.toHaveBeenCalled();
});

test('fetcher: reports a failed submission and restores the instruction', async () => {
  const onCreateFailed = vi.fn();
  const client = makeClient({
    createTask: () =>
      Effect.fail(new ApiError(400, 'no agent is named "ghost"')),
  });
  const fetcher = makeControlPlaneFetcher({
    client,
    workspace: null,
    agent: 'ghost',
    onCreateFailed,
  });
  expect(await collect(fetcher)).toStrictEqual([
    {
      type: EventType.RUN_ERROR,
      message: 'no agent is named "ghost"',
      code: 'create-failed',
    },
  ]);
  expect(onCreateFailed).toHaveBeenCalledWith('run-1', 'Fix the build');
});

test('fetcher: a timed-out submission explains that it may have been accepted', async () => {
  const onCreateFailed = vi.fn();
  const client = makeClient({
    createTask: () =>
      Effect.fail(new ApiError(0, 'the request timed out', 'timeout')),
  });
  const fetcher = makeControlPlaneFetcher({
    client,
    workspace: null,
    agent: 'ghost',
    onCreateFailed,
  });
  expect(await collect(fetcher)).toStrictEqual([
    {
      type: EventType.RUN_ERROR,
      message: timeoutMessage(),
      code: 'create-failed',
    },
  ]);
  expect(timeoutMessage()).toBe(
    `The submission timed out after ${
      REQUEST_TIMEOUT_MS / 1000
    } s. It may have been accepted; check the workspace before resending.`,
  );
  expect(timeoutMessage()).toContain(`${REQUEST_TIMEOUT_MS / 1000} s`);
  expect(timeoutMessage()).toContain('may have been accepted');
  expect(onCreateFailed).toHaveBeenCalledWith('run-1', 'Fix the build');
});

test('fetcher: creates a task, streams it into one assistant message, and finishes it', async () => {
  const createTask = vi.fn(() => Effect.succeed({ id: 'id-1' }));
  const client = makeClient({
    createTask,
    fetchJournal: () =>
      Effect.succeed({
        entries: [message(1, 'working'), toolCall(2, 'inspect', 'completed')],
        total: 2,
      }),
  });
  const created: string[] = [];
  const finished: string[] = [];
  const fetcher = makeControlPlaneFetcher({
    client,
    workspace: '/work/a',
    agent: 'scripted',
    onTaskCreated: (id) => created.push(id),
    onFinished: (id) => finished.push(id),
  });

  const chunks = await collect(fetcher);

  expect(createTask).toHaveBeenCalledWith({
    title: 'Fix the build',
    instruction: 'Fix the build',
    agent: 'scripted',
    workspace: '/work/a',
  });
  expect(created).toStrictEqual(['id-1']);
  expect(finished).toStrictEqual(['id-1']);
  expect(chunks[0]).toStrictEqual({
    type: EventType.RUN_STARTED,
    threadId: THREAD_ID,
    runId: 'run-1',
    timestamp: expect.any(Number),
  });
  expect(
    chunks.filter((c) => c.type === EventType.TEXT_MESSAGE_START),
  ).toStrictEqual([
    {
      type: EventType.TEXT_MESSAGE_START,
      messageId: 'task:id-1:assistant',
      role: 'assistant',
      metadata: { taskId: 'id-1' },
    },
  ]);
  expect(chunks.at(-1)).toStrictEqual({
    type: EventType.RUN_FINISHED,
    threadId: THREAD_ID,
    runId: 'run-1',
    result: { taskId: 'id-1', status: 'completed', outcome: 'done' },
  });
  const text = chunks
    .filter((chunk) => chunk.type === EventType.TEXT_MESSAGE_CONTENT)
    .map((chunk) => chunk.delta)
    .join('');
  expect(text).toBe('working');
  expect(
    chunks.filter((chunk) => chunk.type === EventType.TOOL_CALL_START),
  ).toHaveLength(1);
  expect(chunks.some((chunk) => chunk.type === EventType.TOOL_CALL_ARGS)).toBe(
    true,
  );
  expect(chunks.some((chunk) => chunk.type === EventType.TOOL_CALL_END)).toBe(
    true,
  );
  expect(
    chunks.some((chunk) => chunk.type === EventType.TOOL_CALL_RESULT),
  ).toBe(true);
});

test('fetcher: an empty journal emits no text message', async () => {
  const chunks = await collect(
    makeControlPlaneFetcher({
      client: makeClient(),
      workspace: null,
      agent: 'scripted',
    }),
  );
  expect(chunks.map((chunk) => chunk.type)).toStrictEqual([
    EventType.RUN_STARTED,
    EventType.RUN_FINISHED,
  ]);
});

test('fetcher: a failed submission without a callback still reports the error', async () => {
  const client = makeClient({
    createTask: () => Effect.fail(new ApiError(400, 'bad agent')),
  });
  const chunks = await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
    }),
  );
  expect(chunks).toStrictEqual([
    { type: EventType.RUN_ERROR, message: 'bad agent', code: 'create-failed' },
  ]);
});

test('fetcher: streams several text entries into one assistant message', async () => {
  const client = makeClient({
    fetchJournal: () =>
      Effect.succeed({
        entries: [message(1, 'one'), message(2, 'two')],
        total: 2,
      }),
  });
  const chunks = await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
    }),
  );
  expect(
    chunks.filter((c) => c.type === EventType.TEXT_MESSAGE_START),
  ).toHaveLength(1);
  expect(
    chunks.filter((c) => c.type === EventType.TEXT_MESSAGE_CONTENT),
  ).toHaveLength(2);
  expect(
    chunks.filter((c) => c.type === EventType.TEXT_MESSAGE_END),
  ).toHaveLength(1);
});

test('fetcher: streams a thought as assistant text', async () => {
  const client = makeClient({
    fetchJournal: () =>
      Effect.succeed({ entries: [thought(1, 'thinking')], total: 1 }),
  });
  const chunks = await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
    }),
  );
  expect(chunks[1]).toStrictEqual({
    type: EventType.TEXT_MESSAGE_START,
    messageId: 'task:id-1:assistant',
    role: 'assistant',
    metadata: { taskId: 'id-1' },
  });
  expect(chunks[2]).toStrictEqual({
    type: EventType.TEXT_MESSAGE_CONTENT,
    messageId: 'task:id-1:assistant',
    delta: 'thinking',
  });
  expect(chunks.at(-2)).toStrictEqual({
    type: EventType.TEXT_MESSAGE_END,
    messageId: 'task:id-1:assistant',
  });
});

test('fetcher: finishes with a null outcome when the task has none', async () => {
  const client = makeClient({
    fetchTask: () =>
      Effect.succeed({
        state: {
          spec: {
            task_id: 'id-1',
            title: 'Fix the build',
            instruction: 'Fix the build',
            agent: 'scripted',
          },
          status: 'completed',
          update_count: 0,
        },
      }),
  });
  const chunks = await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
    }),
  );
  expect(chunks.at(-1)).toStrictEqual({
    type: EventType.RUN_FINISHED,
    threadId: THREAD_ID,
    runId: 'run-1',
    result: { taskId: 'id-1', status: 'completed', outcome: null },
  });
});

test('fetcher: omits a null workspace from the submission', async () => {
  const createTask = vi.fn(() => Effect.succeed({ id: 'id-1' }));
  const client = makeClient({ createTask });
  await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
    }),
  );
  expect(createTask).toHaveBeenCalledWith({
    title: 'Fix the build',
    instruction: 'Fix the build',
    agent: 'scripted',
  });
});

test('fetcher: polls until the task is terminal', async () => {
  let polls = 0;
  const client = makeClient({
    fetchTask: () => {
      polls += 1;
      const status: TaskStatus = polls < 3 ? 'running' : 'completed';
      return Effect.succeed({
        state: {
          spec: {
            task_id: 'id-1',
            title: 'Fix the build',
            instruction: 'Fix the build',
            agent: 'scripted',
          },
          status,
          update_count: 0,
          ...(status === 'completed' ? { outcome: 'done' } : {}),
        },
      });
    },
  });
  const chunks = await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
    }),
  );
  expect(polls).toBe(3);
  expect(chunks.filter((c) => c.type === EventType.RUN_FINISHED)).toHaveLength(
    1,
  );
});

test('fetcher: streams each journal entry once across polls', async () => {
  let polls = 0;
  const client = makeClient({
    fetchJournal: () =>
      Effect.succeed({ entries: [message(1, 'once')], total: 1 }),
    fetchTask: () => {
      polls += 1;
      return Effect.succeed({
        state: {
          spec: {
            task_id: 'id-1',
            title: 'Fix the build',
            instruction: 'Fix the build',
            agent: 'scripted',
          },
          status: polls < 2 ? 'running' : 'completed',
          update_count: 0,
          outcome: 'done',
        },
      });
    },
  });
  const chunks = await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
    }),
  );
  expect(
    chunks.filter((c) => c.type === EventType.TEXT_MESSAGE_CONTENT),
  ).toHaveLength(1);
});

test('fetcher: retries a failed poll and then finishes', async () => {
  let attempts = 0;
  const client = makeClient({
    fetchJournal: () => {
      attempts += 1;
      if (attempts === 1) {
        return Effect.fail(new ApiError(500, 'transient'));
      }
      return Effect.succeed({ entries: [message(1, 'working')], total: 1 });
    },
  });
  const chunks = await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
    }),
  );
  expect(chunks.at(-1)?.type).toBe(EventType.RUN_FINISHED);
  expect(attempts).toBe(2);
});

test('fetcher: gives up after the default number of poll failures', async () => {
  let attempts = 0;
  const client = makeClient({
    fetchJournal: () => {
      attempts += 1;
      return Effect.fail(new ApiError(500, 'down'));
    },
  });
  const chunks = await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
    }),
  );
  expect(attempts).toBe(3);
  expect(chunks.at(-1)).toStrictEqual({
    type: EventType.RUN_ERROR,
    message: 'down',
    code: 'poll-failed',
  });
});

test('fetcher: gives up after a configured number of poll failures', async () => {
  let attempts = 0;
  const client = makeClient({
    fetchJournal: () => {
      attempts += 1;
      return Effect.fail(new ApiError(500, 'down'));
    },
  });
  const chunks = await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
      maxPollFailures: 2,
    }),
  );
  expect(attempts).toBe(2);
  expect(chunks.at(-1)).toMatchObject({ code: 'poll-failed' });
});

test('fetcher: does nothing when the run is already aborted', async () => {
  const createTask = vi.fn(() => Effect.succeed({ id: 'id-1' }));
  const client = makeClient({ createTask });
  const fetcher = makeControlPlaneFetcher({
    client,
    workspace: null,
    agent: 'scripted',
  });
  const controller = new AbortController();
  controller.abort();
  expect(await collect(fetcher, INPUT, controller.signal)).toStrictEqual([]);
  expect(createTask).not.toHaveBeenCalled();
});

test('fetcher: a plain abort leaves the created task running', async () => {
  const controller = new AbortController();
  const cancelTask = vi.fn(() => Effect.succeed({ id: 'id-1' }));
  const client = makeClient({ cancelTask });
  const fetcher = makeControlPlaneFetcher({
    client,
    workspace: null,
    agent: 'scripted',
    onTaskCreated: () => {
      controller.abort();
    },
  });
  expect(await collect(fetcher, INPUT, controller.signal)).toStrictEqual([]);
  expect(cancelTask).not.toHaveBeenCalled();
});

test('fetcher: an abort after the terminal chunk does not cancel', async () => {
  const controller = new AbortController();
  const cancelTask = vi.fn(() => Effect.succeed({ id: 'id-1' }));
  const client = makeClient({ cancelTask });
  const fetcher = makeControlPlaneFetcher({
    client,
    workspace: null,
    agent: 'scripted',
  });
  vi.useFakeTimers();
  try {
    const result = await fetcher(INPUT, { signal: controller.signal });
    if (result instanceof Response) {
      throw new Error('the fetcher returned a response');
    }
    const iterator = result[Symbol.asyncIterator]();
    for (;;) {
      const step = await iterator.next();
      if (step.done || step.value.type === EventType.RUN_FINISHED) {
        break;
      }
    }
    controller.abort();
    await iterator.return?.();
    expect(cancelTask).not.toHaveBeenCalled();
  } finally {
    vi.useRealTimers();
  }
});

test('fetcher: an abort between polls ends the loop', async () => {
  const controller = new AbortController();
  let journals = 0;
  const client = makeClient({
    fetchJournal: () => {
      journals += 1;
      controller.abort();
      return Effect.succeed({ entries: [], total: 0 });
    },
    fetchTask: () =>
      Effect.succeed({
        state: {
          spec: {
            task_id: 'id-1',
            title: 'Fix the build',
            instruction: 'Fix the build',
            agent: 'scripted',
          },
          status: 'running',
          update_count: 0,
        },
      }),
  });
  const chunks = await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
    }),
    INPUT,
    controller.signal,
  );
  expect(journals).toBe(1);
  expect(chunks.map((chunk) => chunk.type)).toStrictEqual([
    EventType.RUN_STARTED,
  ]);
});

test('fetcher: a plain abort during a chunk leaves the task running', async () => {
  const controller = new AbortController();
  const cancelTask = vi.fn(() => Effect.succeed({ id: 'id-1' }));
  const client = makeClient({
    cancelTask,
    fetchJournal: () =>
      Effect.succeed({ entries: [message(1, 'working')], total: 1 }),
    fetchTask: () =>
      Effect.succeed({
        state: {
          spec: {
            task_id: 'id-1',
            title: 'Fix the build',
            instruction: 'Fix the build',
            agent: 'scripted',
          },
          status: 'running',
          update_count: 0,
        },
      }),
  });
  const fetcher = makeControlPlaneFetcher({
    client,
    workspace: null,
    agent: 'scripted',
  });
  vi.useFakeTimers();
  try {
    const result = await fetcher(INPUT, { signal: controller.signal });
    if (result instanceof Response) {
      throw new Error('the fetcher returned a response');
    }
    const iterator = result[Symbol.asyncIterator]();
    await iterator.next();
    await iterator.next();
    await iterator.next();
    controller.abort();
    await iterator.return?.();
    expect(cancelTask).not.toHaveBeenCalled();
  } finally {
    vi.useRealTimers();
  }
});

test('fetcher: requests the full journal window and the task detail', async () => {
  const fetchJournal = vi.fn(() => Effect.succeed({ entries: [], total: 0 }));
  const fetchTask = vi.fn(() =>
    Effect.succeed({
      state: {
        spec: {
          task_id: 'id-1',
          title: 'Fix the build',
          instruction: 'Fix the build',
          agent: 'scripted',
        },
        status: 'completed' as const,
        update_count: 0,
        outcome: 'done',
      },
    }),
  );
  const client = makeClient({ fetchJournal, fetchTask });
  await collect(
    makeControlPlaneFetcher({
      client,
      workspace: null,
      agent: 'scripted',
    }),
  );
  expect(fetchJournal).toHaveBeenCalledWith('id-1', JOURNAL_LIMIT);
  expect(fetchTask).toHaveBeenCalledWith('id-1');
});

test('fetcher: property: a run opens once, ends once, and keeps its text', async () => {
  await fc.assert(
    fc.asyncProperty(
      fc.uniqueArray(entryArbitrary(), {
        maxLength: 12,
        selector: (entry) => entry.seq,
      }),
      async (entries) => {
        const client = makeClient({
          fetchJournal: () =>
            Effect.succeed({ entries, total: entries.length }),
        });
        const chunks = await collect(
          makeControlPlaneFetcher({
            client,
            workspace: null,
            agent: 'scripted',
          }),
        );
        expect(chunks[0]?.type).toBe(EventType.RUN_STARTED);
        const terminals = chunks.filter(
          (chunk) =>
            chunk.type === EventType.RUN_FINISHED ||
            chunk.type === EventType.RUN_ERROR,
        );
        expect(terminals).toHaveLength(1);
        expect(chunks.at(-1)?.type).toBe(EventType.RUN_FINISHED);

        const started = new Set<string>();
        const ended = new Set<string>();
        const deltas = new Map<string, string>();
        for (const chunk of chunks) {
          if (chunk.type === EventType.TEXT_MESSAGE_START) {
            started.add(chunk.messageId);
          }
          if (chunk.type === EventType.TEXT_MESSAGE_CONTENT) {
            expect(started.has(chunk.messageId)).toBe(true);
            expect(ended.has(chunk.messageId)).toBe(false);
            deltas.set(
              chunk.messageId,
              `${deltas.get(chunk.messageId) ?? ''}${chunk.delta}`,
            );
          }
          if (chunk.type === EventType.TEXT_MESSAGE_END) {
            ended.add(chunk.messageId);
          }
        }
        const expected = entries
          .map((entry) =>
            entry.kind === 'message' || entry.kind === 'thought'
              ? entry.text
              : '',
          )
          .join('');
        expect(deltas.get('task:id-1:assistant') ?? '').toBe(expected);

        const opened = new Set<string>();
        for (const chunk of chunks) {
          if (chunk.type === EventType.TOOL_CALL_START) {
            opened.add(chunk.toolCallId);
          }
          if (
            chunk.type === EventType.TOOL_CALL_ARGS ||
            chunk.type === EventType.TOOL_CALL_END ||
            chunk.type === EventType.TOOL_CALL_RESULT
          ) {
            expect(opened.has(chunk.toolCallId)).toBe(true);
          }
        }
      },
    ),
  );
});
