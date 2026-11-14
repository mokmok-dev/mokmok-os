import type { Plugin } from '@mokmok/plugin-sdk';

const plugin: Plugin = async (context) => {
  context.emit({ kind: 'message', text: 'waiting to be stopped' });
  await new Promise<void>((resolve) => {
    context.signal.addEventListener('abort', () => resolve(), { once: true });
  });
  return { kind: 'cancelled' };
};

export default plugin;
