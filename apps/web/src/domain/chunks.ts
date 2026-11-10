import { EventType, type StreamChunk } from '@tanstack/ai';

import type { CallEntry, TaskStatus } from './types';

export interface RunResult {
  taskId: string;
  status: TaskStatus;
  outcome: string | null;
}

export function assistantMessageId(taskId: string): string {
  return `task:${taskId}:assistant`;
}

export function textStart(taskId: string): StreamChunk {
  return {
    type: EventType.TEXT_MESSAGE_START,
    messageId: assistantMessageId(taskId),
    role: 'assistant',
    metadata: { taskId },
  };
}

export function textContent(taskId: string, text: string): StreamChunk {
  return {
    type: EventType.TEXT_MESSAGE_CONTENT,
    messageId: assistantMessageId(taskId),
    delta: text,
  };
}

export function textEnd(taskId: string): StreamChunk {
  return {
    type: EventType.TEXT_MESSAGE_END,
    messageId: assistantMessageId(taskId),
  };
}

export function toolChunks(taskId: string, entry: CallEntry): StreamChunk[] {
  const toolCallId = `task:${taskId}:tool:${entry.seq}`;
  return [
    {
      type: EventType.TOOL_CALL_START,
      toolCallId,
      toolCallName: entry.title,
    },
    {
      type: EventType.TOOL_CALL_ARGS,
      toolCallId,
      delta: JSON.stringify({
        title: entry.title,
        status: entry.status,
        kind: entry.kind,
      }),
    },
    { type: EventType.TOOL_CALL_END, toolCallId },
    {
      type: EventType.TOOL_CALL_RESULT,
      messageId: `task:${taskId}:tool-result:${entry.seq}`,
      toolCallId,
      content: entry.status,
      role: 'tool',
    },
  ];
}

export function runStarted(
  threadId: string,
  runId: string,
  timestamp: number,
): StreamChunk {
  return { type: EventType.RUN_STARTED, threadId, runId, timestamp };
}

export function runFinished(
  threadId: string,
  runId: string,
  result: RunResult,
): StreamChunk {
  return { type: EventType.RUN_FINISHED, threadId, runId, result };
}

export function runError(message: string, code?: string): StreamChunk {
  return code === undefined
    ? { type: EventType.RUN_ERROR, message }
    : { type: EventType.RUN_ERROR, message, code };
}
