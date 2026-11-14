import type { Plugin } from '@mokmok/plugin-sdk';

const plugin: Plugin = async (context) => {
  context.emit({ kind: 'message', text: 'ignoring the signal' });
  await new Promise<void>((resolve) => {
    setTimeout(resolve, 30_000);
  });
  return { kind: 'completed', summary: 'never reached' };
};

export default plugin;
