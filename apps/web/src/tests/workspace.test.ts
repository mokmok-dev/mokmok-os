import fc from 'fast-check';
import { test, expect } from 'vitest';

import {
  MAX_WORKSPACE_BYTES,
  NO_WORKSPACE_LABEL,
  SAVED_WORKSPACES_KEY,
  decodeSavedWorkspaces,
  groupSessions,
  normalizeWorkspacePath,
  validateWorkspacePath,
  workspaceFromPath,
  workspaceKey,
  workspaceLabel,
} from '../domain/workspace';
import { makeTask, uuidV7 } from './fixtures';

const encoder = new TextEncoder();

test('workspace: names the storage key and the missing label', () => {
  expect(SAVED_WORKSPACES_KEY).toBe('mokmok.workspaces.v1');
  expect(NO_WORKSPACE_LABEL).toBe('No workspace');
});

test('workspace: labels a path by its basename', () => {
  expect(workspaceLabel('/home/me/project')).toBe('project');
});

test('workspace: ignores trailing slashes when labelling', () => {
  expect(workspaceLabel('/home/me/project///')).toBe('project');
});

test('workspace: labels the root as a slash', () => {
  expect(workspaceLabel('/')).toBe('/');
});

test('workspace: labels a missing workspace', () => {
  expect(workspaceLabel(null)).toBe('No workspace');
});

test('workspace: normalizes a path by dropping trailing slashes', () => {
  expect(normalizeWorkspacePath('/work/demo/')).toBe('/work/demo');
  expect(normalizeWorkspacePath('/work/demo///')).toBe('/work/demo');
  expect(normalizeWorkspacePath('/work/demo')).toBe('/work/demo');
  expect(normalizeWorkspacePath('/')).toBe('/');
});

test('workspace: keys a path by its normalized form', () => {
  expect(workspaceKey('/a/')).toBe('/a');
  expect(workspaceKey('/a')).toBe('/a');
  expect(workspaceKey(null)).toBe('');
});

test('workspace: builds a workspace from a normalized path', () => {
  expect(workspaceFromPath('/a/b/')).toEqual({
    key: '/a/b',
    path: '/a/b',
    label: 'b',
  });
});

test('workspace: builds a workspace from no path', () => {
  expect(workspaceFromPath(null)).toEqual({
    key: '',
    path: null,
    label: NO_WORKSPACE_LABEL,
  });
});

test('workspace: accepts an absolute path at the byte boundary', () => {
  expect(
    validateWorkspacePath(`/${'a'.repeat(MAX_WORKSPACE_BYTES - 1)}`),
  ).toBeNull();
});

test('workspace: rejects a relative path', () => {
  expect(validateWorkspacePath('relative/path')).toBe(
    'The workspace must be an absolute path.',
  );
});

test('workspace: rejects a path over the byte budget', () => {
  expect(validateWorkspacePath(`/${'a'.repeat(MAX_WORKSPACE_BYTES)}`)).toBe(
    `The workspace path must be at most ${MAX_WORKSPACE_BYTES} bytes.`,
  );
});

test('workspace: rejects a path with NUL', () => {
  expect(validateWorkspacePath('/work\0path')).toBe(
    'The workspace path must not contain NUL.',
  );
});

test('workspace: rejects a path with a newline', () => {
  expect(validateWorkspacePath('/work\npath')).toBe(
    'The workspace path must not contain a line break.',
  );
});

test('workspace: rejects a path with a carriage return', () => {
  expect(validateWorkspacePath('/work\rpath')).toBe(
    'The workspace path must not contain a line break.',
  );
});

test('workspace: groups tasks under their path', () => {
  const tasks = [
    makeTask({ id: uuidV7(1), workspace: '/work/a' }),
    makeTask({ id: uuidV7(2), workspace: '/work/b' }),
  ];
  const groups = groupSessions(tasks, []);
  expect(groups.map((group) => group.workspace.path)).toEqual([
    '/work/a',
    '/work/b',
  ]);
  expect(groups.map((group) => group.sessions.length)).toEqual([1, 1]);
});

test('workspace: groups a trailing-slash path with its normalized form', () => {
  const tasks = [
    makeTask({ id: uuidV7(1), workspace: '/work/a' }),
    makeTask({ id: uuidV7(2), workspace: '/work/a/' }),
  ];
  const groups = groupSessions(tasks, []);
  expect(groups).toHaveLength(1);
  expect(groups[0]?.workspace.path).toBe('/work/a');
  expect(groups[0]?.sessions).toHaveLength(2);
});

test('workspace: sorts sessions inside a group newest first', () => {
  const tasks = [
    makeTask({ id: uuidV7(1_000), workspace: '/work/a' }),
    makeTask({ id: uuidV7(2_000), workspace: '/work/a' }),
  ];
  const groups = groupSessions(tasks, []);
  expect(groups[0]?.sessions.map((session) => session.id)).toEqual([
    uuidV7(2_000),
    uuidV7(1_000),
  ]);
});

test('workspace: keeps a saved workspace with no sessions', () => {
  const groups = groupSessions([], ['/work/empty']);
  expect(groups).toHaveLength(1);
  expect(groups[0]?.sessions).toEqual([]);
  expect(groups[0]?.workspace.label).toBe('empty');
});

test('workspace: puts saved workspaces before discovered ones', () => {
  const groups = groupSessions(
    [makeTask({ id: uuidV7(1), workspace: '/work/seen' })],
    ['/work/saved'],
  );
  expect(groups.map((group) => group.workspace.path)).toEqual([
    '/work/saved',
    '/work/seen',
  ]);
});

test('workspace: adds a task to a saved workspace', () => {
  const task = makeTask({ id: uuidV7(1), workspace: '/work/saved' });
  const groups = groupSessions([task], ['/work/saved']);
  expect(groups).toHaveLength(1);
  expect(groups[0]?.sessions).toHaveLength(1);
});

test('workspace: lists a saved workspace once', () => {
  const groups = groupSessions([], ['/work/a', '/work/a/', '/work/a']);
  expect(groups).toHaveLength(1);
});

test('workspace: keeps tasks without a workspace in their own group last', () => {
  const groups = groupSessions(
    [
      makeTask({ id: uuidV7(1), workspace: null }),
      makeTask({ id: uuidV7(2), workspace: '/work/a' }),
    ],
    [],
  );
  expect(groups.map((group) => group.workspace.path)).toEqual([
    '/work/a',
    null,
  ]);
});

test('workspace: orders discovered groups by path', () => {
  const groups = groupSessions(
    [
      makeTask({ id: uuidV7(1), workspace: '/work/z' }),
      makeTask({ id: uuidV7(2), workspace: '/work/a' }),
    ],
    [],
  );
  expect(groups.map((group) => group.workspace.path)).toEqual([
    '/work/a',
    '/work/z',
  ]);
});

test('workspace: property: grouping loses no task and duplicates none', () => {
  fc.assert(
    fc.property(
      fc.array(
        fc.record({
          timestamp: fc.integer({ min: 1_000_000, max: 2 ** 48 - 1 }),
          workspace: fc.option(
            fc.constantFrom('/work/a', '/work/b', '/work/c/'),
            { nil: null },
          ),
        }),
        { maxLength: 40 },
      ),
      fc.array(fc.constantFrom('/work/a', '/work/b/', '/work/saved'), {
        maxLength: 6,
      }),
      (taskOptions, saved) => {
        const tasks = taskOptions.map((option, index) =>
          makeTask({
            id: uuidV7(
              option.timestamp,
              `8000-${String(index).padStart(12, '0')}`,
            ),
            workspace: option.workspace,
          }),
        );
        const groups = groupSessions(tasks, saved);
        const sessionIds = groups.flatMap((group) =>
          group.sessions.map((session) => session.id),
        );
        expect(sessionIds).toHaveLength(tasks.length);
        expect(new Set(sessionIds).size).toBe(tasks.length);
        for (const group of groups) {
          for (const session of group.sessions) {
            const task = tasks.find((candidate) => candidate.id === session.id);
            expect(group.workspace.path).toBe(
              task?.workspace === undefined
                ? null
                : normalizeWorkspacePath(task.workspace),
            );
          }
        }
      },
    ),
  );
});

test('workspace: property: saved workspaces round-trip normalized and unique', () => {
  fc.assert(
    fc.property(
      fc.array(
        fc
          .string({ minLength: 1, maxLength: 20 })
          .map((name) => `/work/${name}`),
        { maxLength: 8 },
      ),
      (paths) => {
        const encoded = JSON.parse(JSON.stringify(paths));
        const expected = [...new Set(paths.map(normalizeWorkspacePath))];
        expect(decodeSavedWorkspaces(encoded)).toEqual(expected);
      },
    ),
  );
});

test('workspace: decode rejects a malformed list', () => {
  expect(decodeSavedWorkspaces('not-an-array')).toBeNull();
  expect(decodeSavedWorkspaces([1, 2])).toBeNull();
});

test('workspace: decode rejects an invalid path', () => {
  expect(decodeSavedWorkspaces(['/ok', 'relative'])).toBeNull();
});

test('workspace: decode normalizes and dedupes saved paths', () => {
  expect(decodeSavedWorkspaces(['/work/a/', '/work/a', '/work/b'])).toEqual([
    '/work/a',
    '/work/b',
  ]);
});

test('workspace: decode accepts an empty list', () => {
  expect(decodeSavedWorkspaces([])).toEqual([]);
});

test('workspace: validates the byte count in UTF-8', () => {
  const path = `/${'漢'.repeat(1400)}`;
  expect(encoder.encode(path).length).toBeGreaterThan(MAX_WORKSPACE_BYTES);
  expect(validateWorkspacePath(path)).toContain('at most');
});
