import type { MessagePart, ToolCallPart, UIMessage } from '@tanstack/ai-client';

import { sessionFromTask } from './session';
import type { JournalEntry, TaskListEntry } from './types';

export interface TurnMetadata {
  taskId: string;
  status: TaskListEntry['status'];
  outcome: string | null;
}

export function toolCallState(status: string): ToolCallPart['state'] {
  if (status === 'completed') {
    return 'complete';
  }
  if (status === 'failed' || status === 'error') {
    return 'error';
  }
  return 'input-complete';
}

export function partForEntry(taskId: string, entry: JournalEntry): MessagePart {
  if (entry.kind === 'message' || entry.kind === 'thought') {
    return { type: 'text', content: entry.text };
  }
  return {
    type: 'tool-call',
    id: `task:${taskId}:tool:${entry.seq}`,
    name: entry.title,
    arguments: JSON.stringify({
      title: entry.title,
      status: entry.status,
      kind: entry.kind,
    }),
    state: toolCallState(entry.status),
    output: entry.status,
  };
}

export function uiMessagesForWorkspace(
  tasks: ReadonlyArray<TaskListEntry>,
  journalsByTask: Readonly<
    Record<string, ReadonlyArray<JournalEntry> | undefined>
  >,
): UIMessage[] {
  const messages: UIMessage[] = [];
  for (const task of tasks) {
    const session = sessionFromTask(task);
    const metadata: TurnMetadata = {
      taskId: session.id,
      status: session.status,
      outcome: session.outcome,
    };
    messages.push({
      id: `task:${session.id}:user`,
      role: 'user',
      parts: [{ type: 'text', content: session.instruction }],
      metadata,
    });
    messages.push({
      id: `task:${session.id}:assistant`,
      role: 'assistant',
      parts: (journalsByTask[session.id] ?? []).map((entry) =>
        partForEntry(session.id, entry),
      ),
      metadata,
    });
  }
  return messages;
}
