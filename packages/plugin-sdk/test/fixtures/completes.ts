import type { Plugin } from '@mokmok/plugin-sdk';

const plugin: Plugin = async (context) => {
  context.emit({ kind: 'message', text: `running ${context.task.title}` });
  context.emit({ kind: 'thought', text: 'the workspace looks fine' });
  context.emit({
    kind: 'tool_call',
    title: 'read the workspace',
    status: 'completed',
  });
  context.emit({
    kind: 'permission',
    title: 'write the report',
    status: 'allowed',
  });
  return { kind: 'completed', summary: `finished ${context.task.task_id}` };
};

export default plugin;
