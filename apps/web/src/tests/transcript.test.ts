import { test, expect } from 'vitest';

import {
  partForEntry,
  toolCallState,
  uiMessagesForWorkspace,
} from '../domain/transcript';
import { makeTask, message, permission, thought, toolCall } from './fixtures';

test('transcript: builds one user and one assistant message per task', () => {
  const messages = uiMessagesForWorkspace(
    [makeTask({ id: 'id-1', instruction: 'Do it.' })],
    { 'id-1': [message(1, 'done')] },
  );
  expect(messages).toHaveLength(2);
  expect(messages[0]?.role).toBe('user');
  expect(messages[1]?.role).toBe('assistant');
  expect(messages[0]?.id).toBe('task:id-1:user');
  expect(messages[1]?.id).toBe('task:id-1:assistant');
  expect(messages[0]?.parts).toEqual([{ type: 'text', content: 'Do it.' }]);
});

test('transcript: uses the listing instruction for the user turn', () => {
  const messages = uiMessagesForWorkspace(
    [makeTask({ id: 'id-1', instruction: 'Ship the fix.' })],
    {},
  );
  expect(messages[0]?.parts).toEqual([
    { type: 'text', content: 'Ship the fix.' },
  ]);
});

test('transcript: carries the task metadata on both messages', () => {
  const messages = uiMessagesForWorkspace(
    [
      makeTask({
        id: 'id-1',
        title: 'Fix it',
        status: 'completed',
        outcome: 'done',
      }),
    ],
    {},
  );
  expect(messages[0]?.metadata).toEqual({
    taskId: 'id-1',
    status: 'completed',
    outcome: 'done',
  });
  expect(messages[1]?.metadata).toEqual(messages[0]?.metadata);
});

test('transcript: maps messages and thoughts to text parts', () => {
  const messages = uiMessagesForWorkspace([makeTask({ id: 'id-1' })], {
    'id-1': [message(1, 'hello'), thought(2, 'thinking')],
  });
  expect(messages[1]?.parts).toEqual([
    { type: 'text', content: 'hello' },
    { type: 'text', content: 'thinking' },
  ]);
});

test('transcript: keeps journal ordering', () => {
  const messages = uiMessagesForWorkspace([makeTask({ id: 'id-1' })], {
    'id-1': [
      thought(1, 'first'),
      message(2, 'second'),
      toolCall(3, 'run', 'completed'),
    ],
  });
  expect(messages[1]?.parts.map((part) => part.type)).toEqual([
    'text',
    'text',
    'tool-call',
  ]);
});

test('transcript: maps a tool call to a tool-call part', () => {
  const part = partForEntry('id-1', toolCall(7, 'inspect', 'completed'));
  expect(part).toEqual({
    type: 'tool-call',
    id: 'task:id-1:tool:7',
    name: 'inspect',
    arguments: JSON.stringify({
      title: 'inspect',
      status: 'completed',
      kind: 'tool_call',
    }),
    state: 'complete',
    output: 'completed',
  });
});

test('transcript: maps a permission to a tool-call part', () => {
  const part = partForEntry('id-1', permission(8, 'allow read', 'pending'));
  expect(part).toMatchObject({
    type: 'tool-call',
    id: 'task:id-1:tool:8',
    name: 'allow read',
    state: 'input-complete',
  });
});

test('transcript: maps tool status to a call state', () => {
  expect(toolCallState('completed')).toBe('complete');
  expect(toolCallState('failed')).toBe('error');
  expect(toolCallState('error')).toBe('error');
  expect(toolCallState('running')).toBe('input-complete');
});

test('transcript: gives an assistant message no parts for an empty journal', () => {
  const messages = uiMessagesForWorkspace([makeTask({ id: 'id-1' })], {});
  expect(messages[1]?.parts).toEqual([]);
});

test('transcript: keeps task order', () => {
  const messages = uiMessagesForWorkspace(
    [
      makeTask({ id: 'id-1', instruction: 'First' }),
      makeTask({ id: 'id-2', instruction: 'Second' }),
    ],
    {},
  );
  expect(messages.map((message) => message.id)).toEqual([
    'task:id-1:user',
    'task:id-1:assistant',
    'task:id-2:user',
    'task:id-2:assistant',
  ]);
});
