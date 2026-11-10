import type { TaskListEntry, TaskStatus } from './types';

export const MAX_TITLE_BYTES = 200;
export const INSTRUCTION_PREVIEW_BYTES = 4096;
export const FALLBACK_TITLE = 'Untitled session';

export interface Session {
  id: string;
  title: string;
  instruction: string;
  status: TaskStatus;
  outcome: string | null;
  workspace: string | null;
  createdAt: Date | null;
}

function truncateUtf8(text: string, maxBytes: number): string {
  const encoder = new TextEncoder();
  let truncated = '';
  let used = 0;
  for (const character of text) {
    const size = encoder.encode(character).length;
    if (used + size > maxBytes) {
      break;
    }
    truncated += character;
    used += size;
  }
  return truncated;
}

export function firstLineOf(text: string): string {
  const index = text.indexOf('\n');
  return index === -1 ? text : text.slice(0, index);
}

export function titleFromInstruction(instruction: string): string {
  const firstLine = firstLineOf(instruction).replaceAll('\r', '').trim();
  const title = firstLine.length === 0 ? FALLBACK_TITLE : firstLine;
  return truncateUtf8(title, MAX_TITLE_BYTES);
}

export function previewInstruction(instruction: string): {
  text: string;
  truncated: boolean;
} {
  const text = truncateUtf8(instruction, INSTRUCTION_PREVIEW_BYTES);
  return text === instruction
    ? { text, truncated: false }
    : { text: `${text}…`, truncated: true };
}

export function timestampFromUuidV7(id: string): Date | null {
  const hex = id.replaceAll('-', '');
  if (!/^[0-9a-f]{12}7[0-9a-f]{19}$/i.test(hex)) {
    return null;
  }
  return new Date(Number.parseInt(hex.slice(0, 12), 16));
}

export function sessionFromTask(entry: TaskListEntry): Session {
  const title =
    entry.title.trim().length > 0
      ? entry.title
      : titleFromInstruction(entry.instruction);
  return {
    id: entry.id,
    title,
    instruction: entry.instruction,
    status: entry.status,
    outcome: entry.outcome ?? null,
    workspace: entry.workspace ?? null,
    createdAt: timestampFromUuidV7(entry.id),
  };
}

export function compareSessions(left: Session, right: Session): number {
  const leftTime = left.createdAt?.getTime() ?? 0;
  const rightTime = right.createdAt?.getTime() ?? 0;
  if (leftTime !== rightTime) {
    return rightTime - leftTime;
  }
  return right.id.localeCompare(left.id);
}
