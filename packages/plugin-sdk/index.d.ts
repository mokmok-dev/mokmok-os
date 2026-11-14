/**
 * The author surface of a mokmok plugin.
 *
 * Every type that crosses into the daemon is generated from the Rust definitions by
 * `napi build --dts` and lands in `types.gen.d.ts`. This file adds only what cannot be
 * generated: the context object the host builds at run time, and the alias a plugin's
 * default export is typed with.
 */
import type { PluginEvent, PluginResult, PluginTask } from './types.gen';

export type { PluginEvent, PluginResult, PluginTask } from './types.gen';

/** What a plugin receives when it runs. */
export interface PluginContext {
  readonly task: PluginTask;
  /** Records one transcript step. Steps after the run ends are ignored. */
  emit(event: PluginEvent): void;
  /**
   * Aborts when a human cancels the task, or when the run passes its timeout.
   *
   * Pass it to `fetch`, a timer, or `AbortSignal.any` so a plugin waiting on something
   * stops waiting. Ignoring it is safe: the run ends without the plugin.
   */
  readonly signal: AbortSignal;
}

/**
 * A plugin: an async function that does the task's work and says how it ended.
 *
 * Return `{ kind: 'completed', summary }` when the work succeeded. Return
 * `{ kind: 'cancelled' }` to stop early on purpose. Throw for a failure, and the thrown
 * error's stack becomes the task's failure reason.
 */
export type Plugin = (context: PluginContext) => Promise<PluginResult>;
