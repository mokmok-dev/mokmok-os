import type { Plugin } from '@mokmok/plugin-sdk';

const plugin: Plugin = async () => {
  for (;;) {
    // Blocks the event loop, so only killing the process ends it.
  }
  return { kind: 'completed', summary: 'never reached' };
};

export default plugin;
