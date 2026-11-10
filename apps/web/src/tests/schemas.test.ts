import { Option, Schema } from 'effect';
import { test, expect } from 'vitest';

import {
  AgentsResponseSchema,
  CancelTaskResponseSchema,
  CreateTaskRequestSchema,
  CreateTaskResponseSchema,
  ErrorResponseSchema,
  JournalEntrySchema,
  JournalResponseSchema,
  TaskDetailResponseSchema,
  TaskListEntrySchema,
  TaskSpecSchema,
  TaskStateSchema,
  TaskStatusSchema,
  TasksResponseSchema,
  isActive,
  isTerminal,
  type TaskStatus,
} from '../domain/types';
import { makeTask, message, permission, thought, toolCall } from './fixtures';

function decodes(
  schema: Schema.ConstraintDecoder<unknown>,
  input: unknown,
): boolean {
  return Option.isSome(Schema.decodeUnknownOption(schema)(input));
}

test('schemas: accepts every task status', () => {
  for (const status of [
    'new',
    'submitted',
    'running',
    'completed',
    'failed',
    'cancelled',
  ]) {
    expect(Schema.decodeUnknownSync(TaskStatusSchema)(status)).toBe(status);
  }
});

test('schemas: rejects an unknown task status', () => {
  expect(decodes(TaskStatusSchema, 'paused')).toBe(false);
});

test('schemas: accepts a full and a minimal task spec', () => {
  const full = {
    task_id: 'id-1',
    title: 'Fix it',
    instruction: 'Do it.',
    agent: 'scripted',
    host: 'eu',
    workspace: '/work/a',
  };
  expect(Schema.decodeUnknownSync(TaskSpecSchema)(full)).toEqual(full);
  const minimal = {
    task_id: 'id-1',
    title: 'Fix it',
    instruction: 'Do it.',
    agent: 'scripted',
  };
  expect(Schema.decodeUnknownSync(TaskSpecSchema)(minimal)).toEqual(minimal);
});

test('schemas: rejects a task spec with a non-string field', () => {
  expect(
    decodes(TaskSpecSchema, {
      task_id: 1,
      title: 'Fix it',
      instruction: 'Do it.',
      agent: 'scripted',
    }),
  ).toBe(false);
});

test('schemas: accepts a task state with and without a spec', () => {
  const state = {
    spec: {
      task_id: 'id-1',
      title: 'Fix it',
      instruction: 'Do it.',
      agent: 'scripted',
      workspace: '/work/a',
    },
    status: 'completed',
    session: 'session-1',
    update_count: 0,
    outcome: 'done',
  };
  expect(Schema.decodeUnknownSync(TaskStateSchema)(state)).toEqual(state);
  const bare = { status: 'new', update_count: 0 };
  expect(Schema.decodeUnknownSync(TaskStateSchema)(bare)).toEqual(bare);
});

test('schemas: tolerates a legacy updates array on a task state', () => {
  expect(
    decodes(TaskStateSchema, {
      status: 'completed',
      update_count: 2,
      updates: [{ kind: 'message', text: 'hi' }],
    }),
  ).toBe(true);
});

test('schemas: rejects a task state with a bad status or update count', () => {
  expect(decodes(TaskStateSchema, { status: 'paused', update_count: 0 })).toBe(
    false,
  );
  expect(
    decodes(TaskStateSchema, { status: 'new', update_count: 'many' }),
  ).toBe(false);
});

test('schemas: accepts a task listing row and ignores extra server fields', () => {
  const entry = {
    id: 'id-1',
    title: 'Fix it',
    instruction: 'Do it.',
    instruction_truncated: true,
    status: 'completed',
    agent: 'scripted',
    host: 'eu',
    workspace: '/work/a',
    outcome: 'done',
    updates: 2,
  };
  expect(Schema.decodeUnknownSync(TaskListEntrySchema)(entry)).toEqual({
    id: 'id-1',
    title: 'Fix it',
    instruction: 'Do it.',
    status: 'completed',
    workspace: '/work/a',
    outcome: 'done',
  });
});

test('schemas: accepts a minimal task listing row', () => {
  const entry = {
    id: 'id-1',
    title: 'Fix it',
    instruction: 'Do it.',
    status: 'new',
  };
  expect(Schema.decodeUnknownSync(TaskListEntrySchema)(entry)).toEqual(entry);
});

test('schemas: rejects a malformed task listing row', () => {
  expect(decodes(TaskListEntrySchema, { id: 'id-1', title: 'x' })).toBe(false);
  expect(
    decodes(TaskListEntrySchema, {
      id: 'id-1',
      title: 'x',
      instruction: 'y',
      status: 'paused',
    }),
  ).toBe(false);
  expect(
    decodes(TaskListEntrySchema, {
      id: 'id-1',
      title: 'x',
      instruction: 'y',
      status: 'new',
      workspace: 1,
    }),
  ).toBe(false);
});

test('schemas: accepts a task list', () => {
  const entry = makeTask({ id: 'id-1' });
  expect(
    Schema.decodeUnknownSync(TasksResponseSchema)({ tasks: [entry] }),
  ).toEqual({ tasks: [entry] });
  expect(Schema.decodeUnknownSync(TasksResponseSchema)({ tasks: [] })).toEqual({
    tasks: [],
  });
});

test('schemas: rejects a malformed task list', () => {
  expect(decodes(TasksResponseSchema, { tasks: [{}] })).toBe(false);
  expect(decodes(TasksResponseSchema, {})).toBe(false);
  expect(decodes(TasksResponseSchema, { tasks: 'nope' })).toBe(false);
});

test('schemas: accepts the agent response and tolerates hosts', () => {
  const response = { agents: ['scripted'], default: 'scripted' };
  expect(Schema.decodeUnknownSync(AgentsResponseSchema)(response)).toEqual(
    response,
  );
  expect(decodes(AgentsResponseSchema, { ...response, hosts: ['eu'] })).toBe(
    true,
  );
});

test('schemas: rejects a malformed agent response', () => {
  expect(decodes(AgentsResponseSchema, { agents: 'nope', default: 'x' })).toBe(
    false,
  );
  expect(decodes(AgentsResponseSchema, { agents: [] })).toBe(false);
  expect(decodes(AgentsResponseSchema, [])).toBe(false);
});

test('schemas: accepts every journal entry kind', () => {
  for (const entry of [
    message(1, 'hello'),
    thought(2, 'hmm'),
    toolCall(3, 'run', 'completed'),
    permission(4, 'allow', 'approved'),
  ]) {
    expect(Schema.decodeUnknownSync(JournalEntrySchema)(entry)).toEqual(entry);
  }
});

test('schemas: rejects an unknown journal kind', () => {
  expect(decodes(JournalEntrySchema, { kind: 'nope', seq: 1, time: '' })).toBe(
    false,
  );
});

test('schemas: rejects a malformed journal entry', () => {
  expect(
    decodes(JournalEntrySchema, { kind: 'message', seq: 1, time: '' }),
  ).toBe(false);
  expect(
    decodes(JournalEntrySchema, {
      kind: 'tool_call',
      seq: 1,
      time: '',
      title: 'run',
    }),
  ).toBe(false);
  expect(
    decodes(JournalEntrySchema, {
      kind: 'permission',
      seq: '1',
      time: '',
      title: 'a',
      status: 'b',
    }),
  ).toBe(false);
});

test('schemas: accepts a journal response', () => {
  const response = { entries: [message(1, 'hi')], total: 1 };
  expect(Schema.decodeUnknownSync(JournalResponseSchema)(response)).toEqual(
    response,
  );
  expect(
    Schema.decodeUnknownSync(JournalResponseSchema)({ entries: [], total: 0 }),
  ).toEqual({ entries: [], total: 0 });
});

test('schemas: rejects a malformed journal response', () => {
  expect(decodes(JournalResponseSchema, { entries: [], total: '1' })).toBe(
    false,
  );
  expect(
    decodes(JournalResponseSchema, {
      entries: [{ kind: 'nope' }],
      total: 1,
    }),
  ).toBe(false);
});

test('schemas: accepts a task detail and tolerates its event fields', () => {
  const detail = {
    state: {
      spec: {
        task_id: 'id-1',
        title: 'Fix it',
        instruction: 'Do it.',
        agent: 'scripted',
      },
      status: 'completed',
      update_count: 0,
      outcome: 'done',
    },
    events: [{ type: 'dev.mokmok.control.submitted' }],
    event_count: 1,
  };
  expect(Schema.decodeUnknownSync(TaskDetailResponseSchema)(detail)).toEqual({
    state: detail.state,
  });
});

test('schemas: rejects a malformed task detail', () => {
  expect(decodes(TaskDetailResponseSchema, { state: { status: 'new' } })).toBe(
    false,
  );
  expect(decodes(TaskDetailResponseSchema, {})).toBe(false);
});

test('schemas: accepts a create task request', () => {
  const request = {
    title: 'Fix it',
    instruction: 'Do it.',
    agent: 'scripted',
    workspace: '/work/a',
  };
  expect(Schema.decodeUnknownSync(CreateTaskRequestSchema)(request)).toEqual(
    request,
  );
  const minimal = { title: 'Fix it', instruction: 'Do it.', agent: 'scripted' };
  expect(Schema.decodeUnknownSync(CreateTaskRequestSchema)(minimal)).toEqual(
    minimal,
  );
});

test('schemas: rejects a malformed create task request', () => {
  expect(
    decodes(CreateTaskRequestSchema, { instruction: 'Do it.', agent: 'a' }),
  ).toBe(false);
  expect(
    decodes(CreateTaskRequestSchema, {
      title: 1,
      instruction: 'a',
      agent: 'a',
    }),
  ).toBe(false);
});

test('schemas: accepts a create and cancel response', () => {
  expect(
    Schema.decodeUnknownSync(CreateTaskResponseSchema)({ id: 'id-1' }),
  ).toEqual({
    id: 'id-1',
  });
  expect(
    Schema.decodeUnknownSync(CancelTaskResponseSchema)({ id: 'id-1' }),
  ).toEqual({
    id: 'id-1',
  });
});

test('schemas: rejects a malformed create response', () => {
  expect(decodes(CreateTaskResponseSchema, { id: 1 })).toBe(false);
  expect(decodes(CancelTaskResponseSchema, {})).toBe(false);
});

test('schemas: accepts only an error object with a string message', () => {
  expect(
    Schema.decodeUnknownSync(ErrorResponseSchema)({ error: 'boom' }),
  ).toEqual({
    error: 'boom',
  });
  expect(decodes(ErrorResponseSchema, {})).toBe(false);
  expect(decodes(ErrorResponseSchema, { error: 1 })).toBe(false);
});

test('schemas: names the terminal statuses', () => {
  const terminal: TaskStatus[] = ['completed', 'failed', 'cancelled'];
  const active: TaskStatus[] = ['new', 'submitted', 'running'];
  for (const status of terminal) {
    expect(isTerminal(status)).toBe(true);
  }
  for (const status of active) {
    expect(isTerminal(status)).toBe(false);
  }
});

test('schemas: names the active statuses', () => {
  for (const status of ['submitted', 'running'] as const) {
    expect(isActive(status)).toBe(true);
  }
  for (const status of ['new', 'completed', 'failed', 'cancelled'] as const) {
    expect(isActive(status)).toBe(false);
  }
});
