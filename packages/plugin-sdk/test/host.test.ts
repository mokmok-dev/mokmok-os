// Drives the plugin host the way the daemon does: one begin frame in, frames out.
//
// Each test spawns a real Node process and reads real stdout, so it covers the module
// loading, the type stripping, the frame queue, the timeout, and the exit path. No
// daemon and no Rust are involved.

import { afterEach, describe, expect, it } from 'vitest';
import { spawn } from 'node:child_process';
import type { ChildProcessWithoutNullStreams } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import type { PluginEvent, PluginOutcome, PluginTask } from '../types.gen';

const here = dirname(fileURLToPath(import.meta.url));
const HOST = join(here, '..', 'src', 'host.mjs');
const FIXTURES = join(here, 'fixtures');
// A fixture that does not parse is kept out of the typechecked tree, so one broken
// plugin cannot fail the SDK's own typecheck.
const BROKEN = join(here, 'broken');

const TASK: PluginTask = {
  task_id: '018f-task',
  title: 'Say hello',
  instruction: 'Greet the user.',
  workspace: '/work/mokmok',
};

/** One line the host wrote on stdout, as the daemon reads it. */
type HostFrame =
  | { type: 'event'; event: PluginEvent }
  | { type: 'end'; result: PluginOutcome };

interface RunOptions {
  timeoutSecs?: number;
  /** Sends the cancel frame the daemon sends when a human cancels. */
  cancelAfterMs?: number;
  /** Sends SIGKILL, which is what the daemon does to a host that ignores everything. */
  killAfterMs?: number;
  dir?: string;
}

interface RunResult {
  frames: HostFrame[];
  code: number | null;
  signal: NodeJS.Signals | null;
  stderr: string;
}

const running = new Set<ChildProcessWithoutNullStreams>();

afterEach(() => {
  for (const child of running) {
    child.kill('SIGKILL');
  }
  running.clear();
});

/** Runs one fixture and resolves with the frames it wrote. */
function runPlugin(
  fixture: string,
  options: RunOptions = {},
): Promise<RunResult> {
  const {
    timeoutSecs = 5,
    cancelAfterMs,
    killAfterMs,
    dir = FIXTURES,
  } = options;
  const child = spawn(
    process.execPath,
    [
      HOST,
      join(dir, `${fixture}.ts`),
      JSON.stringify({ timeout_secs: timeoutSecs }),
    ],
    { stdio: ['pipe', 'pipe', 'pipe'] },
  );
  running.add(child);

  const frames: HostFrame[] = [];
  let stderr = '';
  let partial = '';

  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (chunk: string) => {
    partial += chunk;
    for (;;) {
      const newline = partial.indexOf('\n');
      if (newline < 0) {
        break;
      }
      const line = partial.slice(0, newline);
      partial = partial.slice(newline + 1);
      if (line.trim() !== '') {
        frames.push(JSON.parse(line) as HostFrame);
      }
    }
  });
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk: string) => {
    stderr += chunk;
  });

  child.stdin.write(`${JSON.stringify({ type: 'begin', task: TASK })}\n`);
  if (cancelAfterMs !== undefined) {
    setTimeout(() => child.stdin.write('{"type":"cancel"}\n'), cancelAfterMs);
  }
  if (killAfterMs !== undefined) {
    setTimeout(() => child.kill('SIGKILL'), killAfterMs);
  }

  return new Promise((resolve) => {
    child.on('exit', (code, signal) => {
      running.delete(child);
      resolve({ frames, code, signal, stderr });
    });
  });
}

/** The one end frame a run must produce. */
function endOf(frames: HostFrame[]): PluginOutcome {
  const ends = frames.filter((frame) => frame.type === 'end');
  expect(ends).toHaveLength(1);
  const [end] = ends;
  if (end === undefined || end.type !== 'end') {
    throw new Error('the run wrote no end frame');
  }
  return end.result;
}

function eventOf(frame: HostFrame | undefined): PluginEvent {
  if (frame === undefined || frame.type !== 'event') {
    throw new Error(`expected an event frame, got ${JSON.stringify(frame)}`);
  }
  return frame.event;
}

describe('the plugin host', () => {
  it('streams every event kind and ends completed', async () => {
    const { frames, code } = await runPlugin('completes');

    expect(frames.map((frame) => frame.type)).toEqual([
      'event',
      'event',
      'event',
      'event',
      'end',
    ]);
    expect(eventOf(frames[0])).toEqual({
      kind: 'message',
      text: 'running Say hello',
    });
    expect(eventOf(frames[1])).toEqual({
      kind: 'thought',
      text: 'the workspace looks fine',
    });
    expect(eventOf(frames[2])).toEqual({
      kind: 'tool_call',
      title: 'read the workspace',
      status: 'completed',
    });
    expect(eventOf(frames[3])).toEqual({
      kind: 'permission',
      title: 'write the report',
      status: 'allowed',
    });
    expect(endOf(frames)).toEqual({
      kind: 'completed',
      summary: 'finished 018f-task',
    });
    expect(code).toBe(0);
  });

  it('gives the plugin the task it was handed', async () => {
    const { frames } = await runPlugin('completes');
    // The fixture echoes the title and the id, so a task that did not reach the plugin
    // shows up as a wrong string rather than a passing test.
    const first = eventOf(frames[0]);
    expect(first.kind === 'message' && first.text).toContain(TASK.title);
    const end = endOf(frames);
    expect(end.kind === 'completed' && end.summary).toContain(TASK.task_id);
  });

  it('keeps the stack of a thrown error as the failure reason', async () => {
    const { frames, code } = await runPlugin('throws');

    expect(frames.map((frame) => frame.type)).toEqual(['event', 'end']);
    const result = endOf(frames);
    expect(result.kind).toBe('failed');
    const reason = result.kind === 'failed' ? result.reason : '';
    expect(reason).toContain('the plugin exploded');
    expect(reason).toContain('throws.ts');
    expect(code).toBe(0);
  });

  it('rejects a result that is not an outcome', async () => {
    const { frames } = await runPlugin('bad_result');
    const result = endOf(frames);
    expect(result.kind).toBe('failed');
    const reason = result.kind === 'failed' ? result.reason : '';
    expect(reason).toContain('returned string');
  });

  it('fails an entry with no default export', async () => {
    const { frames } = await runPlugin('no_default');
    const result = endOf(frames);
    const reason = result.kind === 'failed' ? result.reason : '';
    expect(reason).toContain('has no default export function');
  });

  it('fails an entry that does not parse', async () => {
    const { frames } = await runPlugin('syntax_error', { dir: BROKEN });
    const result = endOf(frames);
    expect(result.kind).toBe('failed');
    const reason = result.kind === 'failed' ? result.reason : '';
    expect(reason).toContain('could not be loaded');
  });

  it('ends cancelled when the plugin observes the signal', async () => {
    const { frames } = await runPlugin('cancelled', { cancelAfterMs: 150 });
    expect(endOf(frames)).toEqual({ kind: 'cancelled' });
  });

  it('ends cancelled even when the plugin ignores the signal', async () => {
    const { frames } = await runPlugin('ignores_cancel', {
      cancelAfterMs: 100,
    });
    expect(endOf(frames)).toEqual({ kind: 'cancelled' });
  });

  it('fails a plugin that never settles at the timeout', async () => {
    const started = Date.now();
    const { frames } = await runPlugin('hangs', { timeoutSecs: 1 });
    const result = endOf(frames);
    expect(result.kind).toBe('failed');
    const reason = result.kind === 'failed' ? result.reason : '';
    expect(reason).toContain('exceeded its 1 second timeout');
    expect(Date.now() - started).toBeLessThan(10_000);
  });

  it('writes only whole frames when the daemon kills the host', async () => {
    const { frames, code, signal } = await runPlugin('ignores_cancel', {
      killAfterMs: 100,
    });
    // A killed host cannot settle. What this pins is that every line it did write parsed
    // as a frame, so the daemon never sees a torn one from a process that died mid-write.
    for (const frame of frames) {
      expect(typeof frame.type).toBe('string');
    }
    expect(code === null || signal !== null || code === 0).toBe(true);
  });
});
