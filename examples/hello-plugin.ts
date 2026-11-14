// A minimal mokmok plugin. Save a table like this and pass it to `--agents`:
//
//   {"agents": {"hello": {"transport": "plugin", "entry": "/abs/path/hello-plugin.ts"}}}
//
// Node strips the type annotations at run time, so this file needs no build step and
// no dependency beyond the SDK's declarations.
import type { Plugin } from '@mokmok/plugin-sdk';

const plugin: Plugin = async (context) => {
  context.emit({ kind: 'message', text: `picked up ${context.task.title}` });
  context.emit({ kind: 'thought', text: 'no work to do yet, so this is the whole run' });
  return { kind: 'completed', summary: 'the hello plugin finished' };
};

export default plugin;
