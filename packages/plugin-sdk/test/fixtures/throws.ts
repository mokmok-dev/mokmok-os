import type { Plugin } from '@mokmok/plugin-sdk';

const plugin: Plugin = async (context) => {
  context.emit({ kind: 'message', text: 'about to fail' });
  throw new Error('the plugin exploded');
};

export default plugin;
