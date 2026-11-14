// The plugin host: one Node process per task run.
//
// The daemon spawns this file as `node host.mjs <entry> <options-json>`, writes one
// `begin` frame and any number of `cancel` frames to stdin, and reads frames from
// stdout. Every line written to stdout is protocol; a plugin's own logging goes to
// stderr, which the daemon reads as a log.
//
// This file holds no domain types. The task, the events, and the result are the shapes
// the Rust side defines; this process moves them and decides only what Rust cannot
// observe: a module that does not load, a thrown value, a timeout, and a returned value
// that is not a result.

import { pathToFileURL } from 'node:url';

// The daemon clips these too, but the host clips first: a summary past the daemon's line
// limit would fail the task instead of being shortened. The bound usually arrives in the
// options, so the Rust constant stays the only definition; this is the fallback for a host
// started by hand.
const DEFAULT_MAX_TEXT_BYTES = 4 * 1024;

// Writes are queued so frames reach the pipe in the order the plugin produced them. A
// `message` followed by an `end` must not arrive swapped.
let writes = Promise.resolve();

/** Queues one frame and resolves once the write has reached the pipe. */
function send(frame) {
  const line = `${JSON.stringify(frame)}\n`;
  writes = writes.then(
    () =>
      new Promise((resolve) => {
        process.stdout.write(line, () => resolve());
      }),
  );
  return writes;
}

/** Shortens `text` to `limit` bytes without splitting a UTF-8 sequence. */
function clip(text, limit) {
  const bytes = Buffer.from(text, 'utf8');
  if (bytes.length <= limit) {
    return text;
  }
  const decoder = new TextDecoder('utf-8', { fatal: true });
  for (let end = limit; end > limit - 4; end -= 1) {
    try {
      return decoder.decode(bytes.subarray(0, end));
    } catch {
      // The cut landed inside a multi-byte character, so try one byte earlier.
    }
  }
  return '';
}

/** The failure reason for a thrown value, keeping the stack an author needs. */
function reasonOf(value, limit) {
  if (value instanceof Error) {
    return clip(
      typeof value.stack === 'string'
        ? value.stack
        : `${value.name}: ${value.message}`,
      limit,
    );
  }
  try {
    return clip(String(value), limit);
  } catch {
    return 'the plugin threw a value that cannot be printed';
  }
}

/**
 * Reads a plugin's return value as an outcome.
 *
 * A plugin is plain JavaScript at the wire, so this is the one place a wrong shape turns
 * into a failure with a reason instead of a frame the daemon rejects.
 */
function resultOf(value, limit) {
  if (typeof value !== 'object' || value === null) {
    return {
      kind: 'failed',
      reason: `the plugin returned ${typeof value} instead of a result`,
    };
  }
  if (value.kind === 'completed' && typeof value.summary === 'string') {
    return { kind: 'completed', summary: clip(value.summary, limit) };
  }
  if (value.kind === 'cancelled') {
    return { kind: 'cancelled' };
  }
  const kind =
    typeof value.kind === 'string' ? JSON.stringify(value.kind) : 'no kind';
  return {
    kind: 'failed',
    reason: `the plugin returned a result with ${kind}; expected {kind: 'completed', summary} or {kind: 'cancelled'}`,
  };
}

/**
 * Runs one plugin, streaming its steps and then exactly one end frame.
 *
 * The run ends on whichever of three things happens first: the plugin returns, the
 * timeout expires, or a human cancels. Cancel settles the run as cancelled at once
 * rather than waiting for the plugin to notice, because the daemon has already decided
 * the task's status and a plugin that ignores its signal must not hold the run open. A
 * plugin that finishes after the timeout is ignored for the same reason.
 */
async function run(entry, task, options, controller) {
  const timeoutSecs = options.timeout_secs;
  const limit = options.max_text_bytes ?? DEFAULT_MAX_TEXT_BYTES;
  let ended = false;
  let timedOut = false;

  const finish = async (result) => {
    if (ended) {
      return;
    }
    ended = true;
    await send({ type: 'end', result });
    process.exit(0);
  };

  const emit = (event) => {
    if (!ended) {
      void send({ type: 'event', event });
    }
  };

  const timeout = new Promise((resolve) => {
    setTimeout(() => {
      timedOut = true;
      resolve({
        kind: 'failed',
        reason: `the plugin exceeded its ${timeoutSecs} second timeout`,
      });
      controller.abort();
    }, timeoutSecs * 1000);
  });

  const cancelled = new Promise((resolve) => {
    controller.signal.addEventListener(
      'abort',
      () => {
        // The timeout resolves its own promise first, so an abort it caused must not
        // also settle the run as cancelled.
        if (!timedOut) {
          resolve({ kind: 'cancelled' });
        }
      },
      { once: true },
    );
  });

  let module;
  try {
    module = await import(pathToFileURL(entry).href);
  } catch (error) {
    await finish({
      kind: 'failed',
      reason: `the plugin entry could not be loaded: ${reasonOf(error, limit)}`,
    });
    return;
  }

  if (typeof module.default !== 'function') {
    await finish({
      kind: 'failed',
      reason: `the plugin entry ${entry} has no default export function`,
    });
    return;
  }

  const context = { task, emit, signal: controller.signal };
  const work = Promise.resolve()
    .then(() => module.default(context))
    .then((value) => resultOf(value, limit))
    .catch((error) =>
      controller.signal.aborted
        ? { kind: 'cancelled' }
        : { kind: 'failed', reason: reasonOf(error, limit) },
    );

  await finish(await Promise.race([work, timeout, cancelled]));
}

function main() {
  const [entry, optionsJson] = process.argv.slice(2);
  if (typeof entry !== 'string' || typeof optionsJson !== 'string') {
    process.stderr.write(
      'usage: node host.mjs <plugin-entry> <options-json>\n',
    );
    process.exit(1);
  }

  let options;
  try {
    options = JSON.parse(optionsJson);
  } catch (error) {
    process.stderr.write(
      `the host options are not JSON: ${reasonOf(error, DEFAULT_MAX_TEXT_BYTES)}\n`,
    );
    process.exit(1);
  }

  const controller = new AbortController();
  let began = false;
  let buffered = '';

  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk) => {
    buffered += chunk;
    for (;;) {
      const newline = buffered.indexOf('\n');
      if (newline < 0) {
        return;
      }
      const line = buffered.slice(0, newline);
      buffered = buffered.slice(newline + 1);
      if (line.trim() === '') {
        continue;
      }
      let frame;
      try {
        frame = JSON.parse(line);
      } catch {
        continue;
      }
      if (frame.type === 'begin' && !began) {
        began = true;
        void run(entry, frame.task, options, controller);
      } else if (frame.type === 'cancel') {
        controller.abort();
      }
    }
  });

  // The daemon holds stdin open for the whole run, so EOF means it is gone and this
  // process must not outlive it.
  process.stdin.on('end', () => {
    process.exit(began ? 0 : 1);
  });
}

main();
