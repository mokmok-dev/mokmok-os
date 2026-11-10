import { Option, Schema } from 'effect';

import { compareSessions, sessionFromTask, type Session } from './session';
import type { TaskListEntry } from './types';

export const SAVED_WORKSPACES_KEY = 'mokmok.workspaces.v1';
export const NO_WORKSPACE_LABEL = 'No workspace';
export const MAX_WORKSPACE_BYTES = 4096;

export interface Workspace {
  key: string;
  path: string | null;
  label: string;
}

export interface WorkspaceGroup {
  workspace: Workspace;
  sessions: Session[];
}

const SavedWorkspacesSchema = Schema.Array(Schema.String);
const NO_WORKSPACE_ORDER = '\uffff';

export function normalizeWorkspacePath(path: string): string {
  return path.replace(/\/+$/, '') || '/';
}

export function workspaceKey(path: string | null): string {
  return path === null ? '' : normalizeWorkspacePath(path);
}

export function workspaceLabel(path: string | null): string {
  if (path === null) {
    return NO_WORKSPACE_LABEL;
  }
  const normalized = normalizeWorkspacePath(path);
  if (normalized === '/') {
    return '/';
  }
  return normalized.slice(normalized.lastIndexOf('/') + 1);
}

export function validateWorkspacePath(path: string): string | null {
  if (!path.startsWith('/')) {
    return 'The workspace must be an absolute path.';
  }
  if (new TextEncoder().encode(path).length > MAX_WORKSPACE_BYTES) {
    return `The workspace path must be at most ${MAX_WORKSPACE_BYTES} bytes.`;
  }
  if (path.includes('\0')) {
    return 'The workspace path must not contain NUL.';
  }
  if (path.includes('\n') || path.includes('\r')) {
    return 'The workspace path must not contain a line break.';
  }
  return null;
}

export function workspaceFromPath(path: string | null): Workspace {
  if (path === null) {
    return { key: '', path: null, label: NO_WORKSPACE_LABEL };
  }
  const normalized = normalizeWorkspacePath(path);
  return {
    key: normalized,
    path: normalized,
    label: workspaceLabel(normalized),
  };
}

function compareWorkspacePaths(
  left: string | null,
  right: string | null,
): number {
  return (left ?? NO_WORKSPACE_ORDER).localeCompare(
    right ?? NO_WORKSPACE_ORDER,
  );
}

export function groupSessions(
  tasks: ReadonlyArray<TaskListEntry>,
  savedWorkspaces: ReadonlyArray<string>,
): WorkspaceGroup[] {
  const groups = new Map<string, WorkspaceGroup>();
  const saved: WorkspaceGroup[] = [];
  for (const path of savedWorkspaces) {
    const workspace = workspaceFromPath(path);
    if (groups.has(workspace.key)) {
      continue;
    }
    const group: WorkspaceGroup = { workspace, sessions: [] };
    groups.set(workspace.key, group);
    saved.push(group);
  }

  const discovered: WorkspaceGroup[] = [];
  for (const task of tasks) {
    const workspace = workspaceFromPath(task.workspace ?? null);
    let group = groups.get(workspace.key);
    if (group === undefined) {
      group = { workspace, sessions: [] };
      groups.set(workspace.key, group);
      discovered.push(group);
    }
    group.sessions.push(sessionFromTask(task));
  }

  discovered.sort((left, right) =>
    compareWorkspacePaths(left.workspace.path, right.workspace.path),
  );
  for (const group of groups.values()) {
    group.sessions.sort(compareSessions);
  }
  return [...saved, ...discovered];
}

export function decodeSavedWorkspaces(raw: unknown): string[] | null {
  const decoded = Schema.decodeUnknownOption(SavedWorkspacesSchema)(raw);
  if (Option.isNone(decoded)) {
    return null;
  }
  const paths = decoded.value;
  const valid = paths.every((path) => validateWorkspacePath(path) === null);
  if (!valid) {
    return null;
  }
  return [...new Set(paths.map(normalizeWorkspacePath))];
}
