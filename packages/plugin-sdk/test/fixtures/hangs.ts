import type { Plugin } from '@mokmok/plugin-sdk';

const plugin: Plugin = async () => {
  await new Promise<void>(() => {
    // Never settles, so the run depends on the host's timeout.
  });
  return { kind: 'completed', summary: 'never reached' };
};

export default plugin;
