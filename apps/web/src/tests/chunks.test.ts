import { EventType } from '@tanstack/ai';
import { test, expect } from 'vitest';

import {
  assistantMessageId,
  runError,
  runFinished,
  runStarted,
  textContent,
  textEnd,
  textStart,
  toolChunks,
} from '../domain/chunks';

test('chunks: names one assistant message per task', () => {
  expect(assistantMessageId('id-1')).toBe('task:id-1:assistant');
});

test('chunks: a message starts the task assistant message', () => {
  expect(textStart('id-1')).toStrictEqual({
    type: EventType.TEXT_MESSAGE_START,
    messageId: 'task:id-1:assistant',
    role: 'assistant',
    metadata: { taskId: 'id-1' },
  });
});

test('chunks: text content targets the task assistant message', () => {
  expect(textContent('id-1', 'hello')).toStrictEqual({
    type: EventType.TEXT_MESSAGE_CONTENT,
    messageId: 'task:id-1:assistant',
    delta: 'hello',
  });
});

test('chunks: text ends the task assistant message', () => {
  expect(textEnd('id-1')).toStrictEqual({
    type: EventType.TEXT_MESSAGE_END,
    messageId: 'task:id-1:assistant',
  });
});

test('chunks: a tool call becomes start, args, end, and result', () => {
  const chunks = toolChunks('id-1', {
    seq: 5,
    time: '',
    kind: 'tool_call',
    title: 'inspect',
    status: 'completed',
  });
  expect(chunks).toStrictEqual([
    {
      type: EventType.TOOL_CALL_START,
      toolCallId: 'task:id-1:tool:5',
      toolCallName: 'inspect',
    },
    {
      type: EventType.TOOL_CALL_ARGS,
      toolCallId: 'task:id-1:tool:5',
      delta: JSON.stringify({
        title: 'inspect',
        status: 'completed',
        kind: 'tool_call',
      }),
    },
    {
      type: EventType.TOOL_CALL_END,
      toolCallId: 'task:id-1:tool:5',
    },
    {
      type: EventType.TOOL_CALL_RESULT,
      messageId: 'task:id-1:tool-result:5',
      toolCallId: 'task:id-1:tool:5',
      content: 'completed',
      role: 'tool',
    },
  ]);
});

test('chunks: a permission entry uses the tool-call shape', () => {
  const chunks = toolChunks('id-1', {
    seq: 6,
    time: '',
    kind: 'permission',
    title: 'allow',
    status: 'approved',
  });
  expect(chunks[1]).toMatchObject({
    type: EventType.TOOL_CALL_ARGS,
    delta: JSON.stringify({
      title: 'allow',
      status: 'approved',
      kind: 'permission',
    }),
  });
});

test('chunks: starts a run with the thread, run, and timestamp', () => {
  expect(runStarted('thread', 'run', 123)).toStrictEqual({
    type: EventType.RUN_STARTED,
    threadId: 'thread',
    runId: 'run',
    timestamp: 123,
  });
});

test('chunks: finishes a run with its result', () => {
  expect(
    runFinished('thread', 'run', {
      taskId: 'id-1',
      status: 'completed',
      outcome: 'done',
    }),
  ).toStrictEqual({
    type: EventType.RUN_FINISHED,
    threadId: 'thread',
    runId: 'run',
    result: { taskId: 'id-1', status: 'completed', outcome: 'done' },
  });
});

test('chunks: reports an error without a code', () => {
  expect(runError('boom')).toStrictEqual({
    type: EventType.RUN_ERROR,
    message: 'boom',
  });
});

test('chunks: reports an error with a code', () => {
  expect(runError('boom', 'create-failed')).toStrictEqual({
    type: EventType.RUN_ERROR,
    message: 'boom',
    code: 'create-failed',
  });
});
