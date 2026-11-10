import type { JournalEntry, TaskListEntry, TaskStatus } from '../domain/types';

export const RUN_ID = 'run-1';
export const THREAD_ID = 'workspace-a';

export interface TaskOptions {
  id?: string;
  title?: string;
  instruction?: string;
  workspace?: string | null;
  status?: TaskStatus;
  outcome?: string | null;
}

export function makeTask(options: TaskOptions = {}): TaskListEntry {
  const workspace =
    options.workspace === undefined ? '/work/a' : options.workspace;
  const outcome = options.outcome === undefined ? null : options.outcome;
  const id = options.id ?? '01900000-0000-7000-8000-000000000001';
  return {
    id,
    title: options.title ?? `Task ${id.slice(-1)}`,
    instruction: options.instruction ?? 'Do the thing.',
    status: options.status ?? 'completed',
    ...(workspace === null ? {} : { workspace }),
    ...(outcome === null ? {} : { outcome }),
  };
}

export function uuidV7(
  timestamp: number,
  suffix = '8000-000000000000',
): string {
  const hex = Math.floor(timestamp).toString(16).padStart(12, '0').slice(-12);
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-7000-${suffix}`;
}

export function message(seq: number, text: string): JournalEntry {
  return { seq, time: '2026-11-09T00:00:00.000Z', kind: 'message', text };
}

export function thought(seq: number, text: string): JournalEntry {
  return { seq, time: '2026-11-09T00:00:00.000Z', kind: 'thought', text };
}

export function toolCall(
  seq: number,
  title: string,
  status: string,
): JournalEntry {
  return {
    seq,
    time: '2026-11-09T00:00:00.000Z',
    kind: 'tool_call',
    title,
    status,
  };
}

export function permission(
  seq: number,
  title: string,
  status: string,
): JournalEntry {
  return {
    seq,
    time: '2026-11-09T00:00:00.000Z',
    kind: 'permission',
    title,
    status,
  };
}
