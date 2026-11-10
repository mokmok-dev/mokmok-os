import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/react';
import userEvent, { type UserEvent } from '@testing-library/user-event';
import { Effect } from 'effect';
import { beforeEach, test, expect, vi } from 'vitest';

import { ApiError, type ApiClient } from '../api/client';
import { App } from '../App';
import { SAVED_WORKSPACES_KEY, NO_WORKSPACE_LABEL } from '../domain/workspace';
import type { JournalEntry, TaskListEntry } from '../domain/types';
import { makeTask, message, toolCall, uuidV7 } from './fixtures';

interface Seed {
  task: TaskListEntry;
  entries?: JournalEntry[];
}

interface Harness {
  client: ApiClient;
  tasks: TaskListEntry[];
  journals: Map<string, JournalEntry[]>;
  cancelled: string[];
  createdIds: Set<string>;
  failCreate: boolean;
  failJournal: boolean;
  stayRunning: boolean;
  hideCreated: boolean;
  completeSubmitted: boolean;
  completeAfterPolls: number;
  agentsCalls: number;
  createGates: Promise<void>[];
  createOutcomes: Array<'ok' | 'fail' | 'timeout' | 'timeout-accepted'>;
  cancelFails: boolean;
  journalCalls: Map<string, number>;
}

function makeHarness(seeds: Seed[] = []): Harness {
  const tasks: TaskListEntry[] = seeds.map((seed) => seed.task);
  const journals = new Map<string, JournalEntry[]>(
    seeds.map((seed) => [seed.task.id, seed.entries ?? []]),
  );
  const polls = new Map<string, number>();
  const createdIds = new Set<string>();
  let created = 0;

  const harness: Harness = {
    tasks,
    journals,
    cancelled: [],
    createdIds,
    failCreate: false,
    failJournal: false,
    stayRunning: false,
    hideCreated: false,
    completeSubmitted: false,
    completeAfterPolls: 2,
    agentsCalls: 0,
    createGates: [],
    createOutcomes: [],
    cancelFails: false,
    journalCalls: new Map(),
    client: undefined as unknown as ApiClient,
  };

  const replace = (id: string, patch: Partial<TaskListEntry>) => {
    const index = harness.tasks.findIndex((task) => task.id === id);
    const current = harness.tasks[index];
    if (current !== undefined) {
      harness.tasks[index] = { ...current, ...patch };
    }
  };

  const client: ApiClient = {
    fetchAgents: () => {
      harness.agentsCalls += 1;
      return Effect.succeed({ agents: ['scripted'], default: 'scripted' });
    },
    fetchTasks: () =>
      Effect.sync(() => {
        harness.tasks = harness.tasks.map((task) => {
          if (harness.completeSubmitted && task.status === 'submitted') {
            return {
              ...task,
              status: 'completed',
              outcome: 'the scripted run completed',
            };
          }
          return task;
        });
        return {
          tasks: harness.tasks.filter(
            (task) => !(harness.hideCreated && harness.createdIds.has(task.id)),
          ),
        };
      }),
    fetchTask: (id) =>
      Effect.sync(() => {
        const task = harness.tasks.find((candidate) => candidate.id === id);
        if (task === undefined) {
          throw new ApiError(404, 'no such task');
        }
        const count = (polls.get(id) ?? 0) + 1;
        polls.set(id, count);
        if (
          !harness.stayRunning &&
          count >= harness.completeAfterPolls &&
          (task.status === 'submitted' || task.status === 'running')
        ) {
          replace(id, {
            status: 'completed',
            outcome: 'the scripted run completed',
          });
        }
        const current =
          harness.tasks.find((candidate) => candidate.id === id) ?? task;
        return {
          state: {
            spec: {
              task_id: id,
              title: current.title,
              instruction: current.instruction,
              agent: 'scripted',
              ...(current.workspace === undefined
                ? {}
                : { workspace: current.workspace }),
            },
            status: current.status,
            update_count: 0,
            ...(current.outcome === undefined
              ? {}
              : { outcome: current.outcome }),
          },
        };
      }),
    fetchJournal: (id) => {
      harness.journalCalls.set(id, (harness.journalCalls.get(id) ?? 0) + 1);
      if (harness.failJournal) {
        return Effect.fail(new ApiError(500, 'the journal could not be read'));
      }
      const entries = harness.journals.get(id) ?? [];
      return Effect.succeed({ entries, total: entries.length });
    },
    createTask: (body) =>
      Effect.gen(function* () {
        const outcome = harness.createOutcomes.shift() ?? 'ok';
        const gate = harness.createGates.shift();
        if (gate !== undefined) {
          yield* Effect.promise(() => gate);
        }
        if (outcome === 'timeout') {
          return yield* Effect.fail(
            new ApiError(0, 'the request timed out', 'timeout'),
          );
        }
        if (harness.failCreate || outcome === 'fail') {
          return yield* Effect.fail(
            new ApiError(400, 'no agent is named "scripted"'),
          );
        }
        created += 1;
        const id = uuidV7(1_760_000_000_000 + created);
        const task = makeTask({
          id,
          title: body.title,
          instruction: body.instruction,
          workspace: body.workspace ?? null,
          status: 'submitted',
          outcome: null,
        });
        harness.tasks.push(task);
        harness.createdIds.add(id);
        harness.journals.set(id, [
          message(1, 'the scripted agent received the task'),
          toolCall(2, 'inspect the workspace', 'completed'),
        ]);
        if (outcome === 'timeout-accepted') {
          return yield* Effect.fail(
            new ApiError(0, 'the request timed out', 'timeout'),
          );
        }
        return { id };
      }),
    cancelTask: (id) => {
      harness.cancelled.push(id);
      if (harness.cancelFails) {
        return Effect.fail(new ApiError(409, 'already finished'));
      }
      replace(id, {
        status: 'cancelled',
        outcome: 'cancelled from the web UI',
      });
      return Effect.succeed({ id });
    },
  };
  harness.client = client;
  return harness;
}

async function addWorkspace(user: UserEvent, path: string): Promise<void> {
  const buttons = await screen.findAllByRole('button', {
    name: 'Add workspace',
  });
  const [first] = buttons;
  expect(first).toBeDefined();
  if (first === undefined) {
    return;
  }
  await user.click(first);
  const dialog = await screen.findByRole('dialog');
  await user.type(within(dialog).getByLabelText('Workspace path'), path);
  await user.click(
    within(dialog).getByRole('button', { name: 'Add workspace' }),
  );
  await waitFor(() => {
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
}

async function send(user: UserEvent, text: string): Promise<void> {
  const composer = screen.getByLabelText('Message');
  await user.clear(composer);
  await user.type(composer, text);
  const sendButton = screen.getByRole('button', { name: 'Send' });
  await waitFor(() => {
    expect(sendButton).toBeEnabled();
  });
  await user.click(sendButton);
}

async function renderApp(harness: Harness): Promise<void> {
  render(<App client={harness.client} />);
  await screen.findByText('Connected');
}

beforeEach(() => {
  window.localStorage.clear();
  window.sessionStorage.clear();
});

test('app: adds a workspace, streams a session, and persists the workspace', async () => {
  const user = userEvent.setup();
  const harness = makeHarness();
  await renderApp(harness);

  expect(await screen.findByText(NO_WORKSPACE_LABEL)).toBeInTheDocument();
  await addWorkspace(user, '/work/demo');

  expect(await screen.findByText('/work/demo')).toBeInTheDocument();
  expect(await screen.findByText('No sessions yet')).toBeInTheDocument();

  await send(user, 'Fix the build');

  expect(
    await screen.findByText('the scripted agent received the task'),
  ).toBeInTheDocument();
  expect(await screen.findByText('inspect the workspace')).toBeInTheDocument();
  expect(await screen.findByText('Completed')).toBeInTheDocument();
  expect((await screen.findAllByText('Fix the build')).length).toBeGreaterThan(
    1,
  );

  const saved = window.localStorage.getItem(SAVED_WORKSPACES_KEY);
  expect(saved === null ? null : JSON.parse(saved)).toEqual(['/work/demo']);
});

test('app: New in a non-empty workspace shows New session with no active row', async () => {
  const user = userEvent.setup();
  const seed = makeTask({
    id: uuidV7(1_760_000_000_000),
    workspace: '/work/a',
    title: 'Older task',
    status: 'completed',
  });
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness([{ task: seed, entries: [message(1, 'old')] }]);
  await renderApp(harness);

  const older = await screen.findByRole('button', { name: /Older task/ });
  await user.click(older);
  await waitFor(() => {
    expect(older).toHaveAttribute('aria-current', 'page');
  });

  await user.click(screen.getByRole('button', { name: 'New session' }));

  expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(
    'New session',
  );
  expect(older).not.toHaveAttribute('aria-current');
  expect(screen.queryByRole('button', { name: 'Cancel' })).toBeNull();
  expect(screen.getByText(/New session —/)).toBeInTheDocument();
});

test('app: a created task shows New session until the listing includes it', async () => {
  const user = userEvent.setup();
  const seed = makeTask({
    id: uuidV7(1_760_000_000_000),
    workspace: '/work/a',
    title: 'Older task',
    status: 'completed',
  });
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness([{ task: seed, entries: [message(1, 'old')] }]);
  harness.hideCreated = true;
  await renderApp(harness);

  await screen.findByRole('button', { name: /Older task/ });
  await send(user, 'Fresh task');

  expect(await screen.findByText('Fresh task')).toBeInTheDocument();
  expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(
    'New session',
  );
  expect(screen.queryByRole('button', { name: 'Cancel' })).toBeNull();
  expect(
    screen.queryByRole('button', { name: /Fresh task/ }),
  ).not.toBeInTheDocument();

  harness.hideCreated = false;
  await screen.findByRole('button', { name: /Fresh task/ }, { timeout: 5_000 });
  expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(
    'Fresh task',
  );
});

test('app: a failed send keeps the instruction recoverable', async () => {
  const user = userEvent.setup();
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness();
  harness.failCreate = true;
  await renderApp(harness);

  await send(user, 'Keep this text');

  expect(
    await screen.findByText(/no agent is named "scripted"/),
  ).toBeInTheDocument();
  expect(screen.getByLabelText('Message')).toHaveValue('Keep this text');
  expect(
    screen.queryByRole('button', { name: /Keep this text/ }),
  ).not.toBeInTheDocument();
});

test('app: a timed-out submission restores the draft and warns', async () => {
  const user = userEvent.setup();
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness();
  harness.createOutcomes.push('timeout');
  await renderApp(harness);

  await send(user, 'Timed out');

  expect(
    await screen.findByText(/The submission timed out after/),
  ).toBeInTheDocument();
  expect(screen.getAllByText(/timed out after 30 s/)).toHaveLength(1);
  expect(screen.queryByText('Submission timed out')).not.toBeInTheDocument();
  expect(screen.getByText(/may have been accepted/)).toBeInTheDocument();
  expect(screen.getByLabelText('Message')).toHaveValue('Timed out');
  expect(
    screen.queryByRole('button', { name: /Timed out/ }),
  ).not.toBeInTheDocument();
});

test('app: a task accepted after a timeout appears in the workspace', async () => {
  const user = userEvent.setup();
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness();
  harness.createOutcomes.push('timeout-accepted');
  await renderApp(harness);

  await send(user, 'Accepted late');

  expect(
    await screen.findByText(/The submission timed out after/),
  ).toBeInTheDocument();
  expect(screen.getAllByText(/timed out after 30 s/)).toHaveLength(1);
  expect(screen.getByLabelText('Message')).toHaveValue('Accepted late');
  expect(
    await screen.findByRole(
      'button',
      { name: /Accepted late/ },
      { timeout: 4_000 },
    ),
  ).toBeInTheDocument();
  expect(harness.tasks.some((task) => task.title === 'Accepted late')).toBe(
    true,
  );
});

test('app: Stop cancels the running task exactly once', async () => {
  const user = userEvent.setup();
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness();
  harness.stayRunning = true;
  await renderApp(harness);

  await send(user, 'Long task');
  const stop = await screen.findByRole('button', { name: 'Stop' });
  await user.click(stop);

  await waitFor(() => {
    expect(harness.cancelled).toHaveLength(1);
  });
  expect(await screen.findByText('Cancelled')).toBeInTheDocument();
});

test('app: switching workspace mid-run does not cancel the task', async () => {
  const user = userEvent.setup();
  const other = makeTask({
    id: uuidV7(1_750_000_000_000),
    workspace: '/work/b',
    title: 'Other task',
    status: 'completed',
  });
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness([
    { task: other, entries: [message(1, 'other transcript')] },
  ]);
  harness.stayRunning = true;
  await renderApp(harness);

  await screen.findByRole('button', { name: /Other task/ });
  await send(user, 'Switch task');
  await screen.findByRole('button', { name: 'Stop' });

  await user.click(screen.getByRole('button', { name: /Other task/ }));
  expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(
    'Other task',
  );

  await new Promise((resolve) => setTimeout(resolve, 300));
  expect(harness.cancelled).toHaveLength(0);
});

test('app: a journal update during a run is applied after the run', async () => {
  const user = userEvent.setup();
  const seed = makeTask({
    id: uuidV7(1_760_000_000_000),
    workspace: '/work/a',
    title: 'Seed task',
    status: 'running',
  });
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness([{ task: seed, entries: [] }]);
  harness.completeAfterPolls = 5;
  await renderApp(harness);

  await screen.findByRole('button', { name: /Seed task/ });
  await send(user, 'Second session');
  await screen.findByRole('button', { name: 'Stop' });

  await new Promise((resolve) => setTimeout(resolve, 300));
  harness.journals.set(seed.id, [message(1, 'late update')]);
  await new Promise((resolve) => setTimeout(resolve, 1_200));
  expect(screen.queryByText('late update')).toBeNull();

  await waitFor(
    () => {
      expect(screen.queryByRole('button', { name: 'Stop' })).toBeNull();
    },
    { timeout: 5_000 },
  );
  expect(await screen.findByText('late update')).toBeInTheDocument();
});

test('app: a journal failure keeps the session and offers a retry', async () => {
  const user = userEvent.setup();
  const seed = makeTask({
    id: uuidV7(1_760_000_000_000),
    workspace: '/work/a',
    title: 'Seed task',
    status: 'completed',
  });
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness([
    { task: seed, entries: [message(1, 'seed transcript')] },
  ]);
  harness.failJournal = true;
  await renderApp(harness);

  expect(
    await screen.findByText(
      /Transcript for Seed task could not be loaded/,
      {},
      { timeout: 5_000 },
    ),
  ).toBeInTheDocument();
  expect(screen.getByRole('button', { name: /Seed task/ })).toBeInTheDocument();

  harness.failJournal = false;
  await user.click(screen.getByRole('button', { name: 'Retry' }));
  expect(await screen.findByText('seed transcript')).toBeInTheDocument();
});

test('app: removes a saved workspace and dedupes trailing slashes', async () => {
  const user = userEvent.setup();
  const harness = makeHarness();
  await renderApp(harness);

  await addWorkspace(user, '/work/demo/');
  await addWorkspace(user, '/work/demo');

  expect(
    screen.getAllByRole('button', { name: 'Remove workspace demo' }),
  ).toHaveLength(1);
  expect(
    JSON.parse(window.localStorage.getItem(SAVED_WORKSPACES_KEY) ?? '[]'),
  ).toEqual(['/work/demo']);

  await user.click(
    screen.getByRole('button', { name: 'Remove workspace demo' }),
  );
  const dialog = await screen.findByRole('alertdialog');
  await user.click(within(dialog).getByRole('button', { name: 'Remove' }));

  expect(await screen.findByText('No workspace selected')).toBeInTheDocument();
  expect(
    JSON.parse(window.localStorage.getItem(SAVED_WORKSPACES_KEY) ?? '[]'),
  ).toEqual([]);
});

test('app: settings validate the URL and clear the token on an origin change', async () => {
  const user = userEvent.setup();
  const harness = makeHarness();
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(
          JSON.stringify({ agents: ['scripted'], default: 'scripted' }),
          { status: 200 },
        ),
    ),
  );
  await renderApp(harness);
  const agentsBefore = harness.agentsCalls;

  await user.click(screen.getByRole('button', { name: 'Connection settings' }));
  const dialog = await screen.findByRole('dialog');
  await user.type(within(dialog).getByLabelText('Bearer token'), 'top-secret');
  await user.type(
    within(dialog).getByLabelText('Control plane URL'),
    'http://other:9000/api',
  );
  await user.click(within(dialog).getByRole('button', { name: 'Save' }));
  expect(
    await within(dialog).findByText(/must name only an origin/),
  ).toBeInTheDocument();

  const url = within(dialog).getByLabelText('Control plane URL');
  await user.clear(url);
  await user.type(url, 'http://other:9000');
  const tokenInput = within(dialog).getByLabelText('Bearer token');
  await user.click(tokenInput);
  expect(tokenInput).toHaveValue('');
  expect(within(dialog).getByText(/token was cleared/)).toBeInTheDocument();

  await user.type(tokenInput, 'new-secret');
  await user.click(within(dialog).getByRole('button', { name: 'Save' }));
  await waitFor(() => {
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  expect(
    JSON.parse(window.sessionStorage.getItem('mokmok.control.token') ?? 'null'),
  ).toEqual({ origin: 'http://other:9000', token: 'new-secret' });
  expect(window.localStorage.getItem('mokmok.connection.v1')).toContain(
    'http://other:9000',
  );
  expect(window.location.href).not.toContain('new-secret');
  expect(window.location.search).toBe('');
  await waitFor(() => {
    expect(harness.agentsCalls).toBeGreaterThan(agentsBefore);
  });
});

test('app: a transient URL edit does not clear the token', async () => {
  const user = userEvent.setup();
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  window.localStorage.setItem(
    'mokmok.connection.v1',
    JSON.stringify({ baseUrl: 'http://127.0.0.1:8787' }),
  );
  window.sessionStorage.setItem(
    'mokmok.control.token',
    JSON.stringify({ origin: 'http://127.0.0.1:8787', token: 'secret' }),
  );
  const harness = makeHarness();
  await renderApp(harness);

  await user.click(screen.getByRole('button', { name: 'Connection settings' }));
  const dialog = await screen.findByRole('dialog');
  const url = within(dialog).getByLabelText('Control plane URL');
  const token = within(dialog).getByLabelText('Bearer token');
  expect(url).toHaveValue('http://127.0.0.1:8787');
  expect(token).toHaveValue('secret');

  await user.clear(url);
  await user.type(url, 'http://127.0.0.1:8787');
  expect(token).toHaveValue('secret');

  await user.clear(token);
  await user.type(token, 'typed-secret');

  await user.clear(url);
  await user.type(url, 'http://other:9000');
  await user.click(token);
  expect(token).toHaveValue('');
  expect(within(dialog).getByText(/token was cleared/)).toBeInTheDocument();

  await user.clear(url);
  await user.type(url, 'http://127.0.0.1:8787');
  expect(token).toHaveValue('typed-secret');
});

test('app: a no-op Save does not remount or refetch', async () => {
  const user = userEvent.setup();
  const harness = makeHarness();
  await renderApp(harness);
  const before = harness.agentsCalls;

  await user.click(screen.getByRole('button', { name: 'Connection settings' }));
  const dialog = await screen.findByRole('dialog');
  await user.click(within(dialog).getByRole('button', { name: 'Save' }));
  await waitFor(() => {
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
  await new Promise((resolve) => setTimeout(resolve, 250));
  expect(harness.agentsCalls).toBe(before);
});

test('app: a failed task renders its outcome', async () => {
  const seed = makeTask({
    id: uuidV7(1_760_000_000_000),
    workspace: '/work/a',
    title: 'Broken task',
    status: 'failed',
    outcome: 'the run broke',
  });
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness([{ task: seed, entries: [] }]);
  await renderApp(harness);

  expect(await screen.findByText('the run broke')).toBeInTheDocument();
});

test('app: Stop before the task id exists cancels the created task once', async () => {
  const user = userEvent.setup();
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness();
  let release: () => void = () => {};
  harness.createGates.push(
    new Promise<void>((resolve) => {
      release = resolve;
    }),
  );
  await renderApp(harness);

  await send(user, 'Stop early');
  await screen.findByRole('button', { name: 'Stop' });
  await user.click(screen.getByRole('button', { name: 'Stop' }));
  release();

  await waitFor(() => {
    expect(harness.cancelled).toHaveLength(1);
  });
  expect(harness.cancelled[0]).toBe(harness.tasks[0]?.id);
  expect(harness.tasks[0]?.status).toBe('cancelled');
});

test('app: a workspace switch during create does not hijack the new selection', async () => {
  const user = userEvent.setup();
  const other = makeTask({
    id: uuidV7(1_750_000_000_000),
    workspace: '/work/b',
    title: 'Other task',
    status: 'completed',
  });
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a', '/work/b']),
  );
  const harness = makeHarness([
    { task: other, entries: [message(1, 'other transcript')] },
  ]);
  let release: () => void = () => {};
  harness.createGates.push(
    new Promise<void>((resolve) => {
      release = resolve;
    }),
  );
  await renderApp(harness);

  const otherSession = await screen.findByRole('button', {
    name: /Other task/,
  });
  await user.click(otherSession);
  await user.click(screen.getByRole('button', { name: 'Add session' }));
  await send(user, 'Late create');
  await screen.findByRole('button', { name: 'Stop' });
  await user.click(screen.getByRole('button', { name: /Other task/ }));
  release();

  await new Promise((resolve) => setTimeout(resolve, 500));
  expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(
    'Other task',
  );
  expect(otherSession).toHaveAttribute('aria-current', 'page');
});

test('app: unpins a non-empty saved workspace without removing its sessions', async () => {
  const user = userEvent.setup();
  const seed = makeTask({
    id: uuidV7(1_760_000_000_000),
    workspace: '/work/a',
    title: 'Pinned task',
    status: 'completed',
  });
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness([{ task: seed, entries: [message(1, 'old')] }]);
  await renderApp(harness);

  await screen.findByRole('button', { name: /Pinned task/ });
  await user.click(screen.getByRole('button', { name: 'Unpin workspace a' }));
  const dialog = await screen.findByRole('alertdialog');
  expect(within(dialog).getByText('Unpin workspace?')).toBeInTheDocument();
  await user.click(within(dialog).getByRole('button', { name: 'Unpin' }));

  expect(
    screen.getByRole('button', { name: /Pinned task/ }),
  ).toBeInTheDocument();
  expect(
    JSON.parse(window.localStorage.getItem(SAVED_WORKSPACES_KEY) ?? '[]'),
  ).toEqual([]);
  expect(
    screen.queryByRole('button', { name: /Unpin workspace/ }),
  ).not.toBeInTheDocument();
});

test('app: a stop before create does not affect the next run', async () => {
  const user = userEvent.setup();
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness();
  let release: () => void = () => {};
  harness.createGates.push(
    new Promise<void>((resolve) => {
      release = resolve;
    }),
  );
  await renderApp(harness);

  await send(user, 'First');
  await screen.findByRole('button', { name: 'Stop' });
  await user.click(screen.getByRole('button', { name: 'Stop' }));

  await send(user, 'Second');
  release();

  await waitFor(() => {
    const first = harness.tasks.find((task) => task.title === 'First');
    const second = harness.tasks.find((task) => task.title === 'Second');
    expect(first?.status).toBe('cancelled');
    expect(second?.status).toBe('completed');
  });
  expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Second');
  const first = harness.tasks.find((task) => task.title === 'First');
  expect(harness.cancelled).toEqual([first?.id]);
});

test('app: a stopped create that fails restores the instruction', async () => {
  const user = userEvent.setup();
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness();
  let fail: (error: Error) => void = () => {};
  harness.createGates.push(
    new Promise<void>((_resolve, reject) => {
      fail = reject;
    }),
  );
  await renderApp(harness);

  await send(user, 'Hanging');
  await screen.findByRole('button', { name: 'Stop' });
  await user.click(screen.getByRole('button', { name: 'Stop' }));
  fail(new Error('timed out'));

  await waitFor(() => {
    expect(screen.getByLabelText('Message')).toHaveValue('Hanging');
  });

  await send(user, 'After failure');
  expect(
    await screen.findByRole('button', { name: /After failure/ }),
  ).toHaveAttribute('aria-current', 'page');
});

test('app: a failed first create does not disturb the second run', async () => {
  const user = userEvent.setup();
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness();
  harness.stayRunning = true;
  harness.createOutcomes.push('fail', 'ok');
  let release: () => void = () => {};
  harness.createGates.push(
    new Promise<void>((resolve) => {
      release = resolve;
    }),
  );
  await renderApp(harness);

  await send(user, 'First failing');
  await screen.findByRole('button', { name: 'Stop' });
  await user.click(screen.getByRole('button', { name: 'Stop' }));

  await send(user, 'Second running');
  await screen.findByRole('button', { name: /Second running/ });
  expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(
    'Second running',
  );

  release();
  await new Promise((resolve) => setTimeout(resolve, 300));

  expect(harness.tasks.map((task) => task.title)).toEqual(['Second running']);
  expect(screen.getByLabelText('Message')).toHaveValue('');
  expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(
    'Second running',
  );
  expect(screen.getByRole('button', { name: 'Stop' })).toBeInTheDocument();
  expect(harness.cancelled).toHaveLength(0);

  const secondId = harness.tasks[0]?.id;
  await user.click(screen.getByRole('button', { name: 'Stop' }));
  await waitFor(() => {
    expect(harness.cancelled).toEqual([secondId]);
  });
});

test('app: a failed cancel still resumes the journal', async () => {
  const user = userEvent.setup();
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness();
  harness.stayRunning = true;
  harness.cancelFails = true;
  await renderApp(harness);

  await send(user, 'Cancel fails');
  await screen.findByRole('button', { name: 'Stop' });
  await user.click(screen.getByRole('button', { name: 'Stop' }));

  await waitFor(() => {
    expect(harness.cancelled).toHaveLength(1);
  });
  const id = harness.cancelled[0] ?? '';
  const before = harness.journalCalls.get(id) ?? 0;
  await new Promise((resolve) => setTimeout(resolve, 1_300));
  expect(harness.journalCalls.get(id) ?? 0).toBeGreaterThan(before);
});

test('app: a create released in the same turn as a workspace switch keeps the new selection', async () => {
  const user = userEvent.setup();
  const other = makeTask({
    id: uuidV7(1_750_000_000_000),
    workspace: '/work/b',
    title: 'Other task',
    status: 'completed',
  });
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a', '/work/b']),
  );
  const harness = makeHarness([
    { task: other, entries: [message(1, 'other transcript')] },
  ]);
  let release: () => void = () => {};
  harness.createGates.push(
    new Promise<void>((resolve) => {
      release = resolve;
    }),
  );
  await renderApp(harness);

  await screen.findByRole('button', { name: /Other task/ });
  await user.click(screen.getByRole('button', { name: 'Add session' }));
  await send(user, 'Same turn');
  await screen.findByRole('button', { name: 'Stop' });

  const otherSession = screen.getByRole('button', { name: /Other task/ });
  fireEvent.click(otherSession);
  release();

  await new Promise((resolve) => setTimeout(resolve, 500));
  expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(
    'Other task',
  );
  expect(screen.getByRole('button', { name: /Other task/ })).toHaveAttribute(
    'aria-current',
    'page',
  );
});

test('app: a long instruction renders the same preview live and after hydration', async () => {
  window.localStorage.setItem(
    SAVED_WORKSPACES_KEY,
    JSON.stringify(['/work/a']),
  );
  const harness = makeHarness();
  await renderApp(harness);

  const instruction = 'a'.repeat(5_000);
  const composer = screen.getByLabelText('Message');
  fireEvent.change(composer, { target: { value: instruction } });
  const sendButton = screen.getByRole('button', { name: 'Send' });
  await waitFor(() => {
    expect(sendButton).toBeEnabled();
  });
  fireEvent.click(sendButton);

  const displayed = () =>
    document.querySelector('.bubble .text-part')?.textContent ?? '';
  await waitFor(() => {
    expect(displayed()).toContain('…');
  });
  expect(displayed()).toHaveLength(4_097);
  expect(screen.getByText(/Instruction preview/)).toBeInTheDocument();

  await screen.findByText('Completed');
  expect(displayed()).toHaveLength(4_097);
  expect(screen.getByText(/Instruction preview/)).toBeInTheDocument();
});

test('app: runs sessions in two workspaces and switches between them', async () => {
  const user = userEvent.setup();
  const harness = makeHarness();
  await renderApp(harness);

  await screen.findByText(NO_WORKSPACE_LABEL);
  await addWorkspace(user, '/work/one');
  await send(user, 'First task');
  expect(await screen.findByText('Completed')).toBeInTheDocument();

  await addWorkspace(user, '/work/two');
  await send(user, 'Second task');
  await waitFor(() => {
    expect(screen.getAllByText('Completed').length).toBeGreaterThan(0);
  });

  expect(await screen.findByText('one')).toBeInTheDocument();
  expect(await screen.findByText('two')).toBeInTheDocument();

  const firstSession = await screen.findByRole('button', {
    name: /First task/,
  });
  await user.click(firstSession);
  expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(
    'First task',
  );
});
