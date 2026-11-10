import { Schema } from 'effect';

export const TaskStatusSchema = Schema.Literals([
  'new',
  'submitted',
  'running',
  'completed',
  'failed',
  'cancelled',
]);
export type TaskStatus = Schema.Schema.Type<typeof TaskStatusSchema>;

export const TaskSpecSchema = Schema.Struct({
  task_id: Schema.String,
  title: Schema.String,
  instruction: Schema.String,
  agent: Schema.String,
  host: Schema.optionalKey(Schema.String),
  workspace: Schema.optionalKey(Schema.String),
});

export const TaskStateSchema = Schema.Struct({
  spec: Schema.optionalKey(TaskSpecSchema),
  status: TaskStatusSchema,
  session: Schema.optionalKey(Schema.String),
  update_count: Schema.Number,
  outcome: Schema.optionalKey(Schema.String),
});

export const TaskListEntrySchema = Schema.Struct({
  id: Schema.String,
  title: Schema.String,
  instruction: Schema.String,
  status: TaskStatusSchema,
  workspace: Schema.optionalKey(Schema.String),
  outcome: Schema.optionalKey(Schema.String),
});
export type TaskListEntry = Schema.Schema.Type<typeof TaskListEntrySchema>;

export const MessageEntrySchema = Schema.Struct({
  seq: Schema.Number,
  time: Schema.String,
  kind: Schema.Literal('message'),
  text: Schema.String,
});
export const ThoughtEntrySchema = Schema.Struct({
  seq: Schema.Number,
  time: Schema.String,
  kind: Schema.Literal('thought'),
  text: Schema.String,
});
export const ToolCallEntrySchema = Schema.Struct({
  seq: Schema.Number,
  time: Schema.String,
  kind: Schema.Literal('tool_call'),
  title: Schema.String,
  status: Schema.String,
});
export const PermissionEntrySchema = Schema.Struct({
  seq: Schema.Number,
  time: Schema.String,
  kind: Schema.Literal('permission'),
  title: Schema.String,
  status: Schema.String,
});
export const JournalEntrySchema = Schema.Union([
  MessageEntrySchema,
  ThoughtEntrySchema,
  ToolCallEntrySchema,
  PermissionEntrySchema,
]);
export type JournalEntry = Schema.Schema.Type<typeof JournalEntrySchema>;
export type CallEntry = Extract<
  JournalEntry,
  { kind: 'tool_call' | 'permission' }
>;

export const AgentsResponseSchema = Schema.Struct({
  agents: Schema.Array(Schema.String),
  default: Schema.String,
});
export type AgentsResponse = Schema.Schema.Type<typeof AgentsResponseSchema>;

export const TasksResponseSchema = Schema.Struct({
  tasks: Schema.Array(TaskListEntrySchema),
});
export type TasksResponse = Schema.Schema.Type<typeof TasksResponseSchema>;

export const JournalResponseSchema = Schema.Struct({
  entries: Schema.Array(JournalEntrySchema),
  total: Schema.Number,
});
export type JournalResponse = Schema.Schema.Type<typeof JournalResponseSchema>;

export const TaskDetailResponseSchema = Schema.Struct({
  state: TaskStateSchema,
});
export type TaskDetailResponse = Schema.Schema.Type<
  typeof TaskDetailResponseSchema
>;

export const CreateTaskRequestSchema = Schema.Struct({
  title: Schema.String,
  instruction: Schema.String,
  agent: Schema.String,
  workspace: Schema.optionalKey(Schema.String),
  host: Schema.optionalKey(Schema.String),
});
export type CreateTaskRequest = Schema.Schema.Type<
  typeof CreateTaskRequestSchema
>;

export const CreateTaskResponseSchema = Schema.Struct({ id: Schema.String });
export type CreateTaskResponse = Schema.Schema.Type<
  typeof CreateTaskResponseSchema
>;

export const CancelTaskResponseSchema = CreateTaskResponseSchema;
export type CancelTaskResponse = Schema.Schema.Type<
  typeof CancelTaskResponseSchema
>;

export const ErrorResponseSchema = Schema.Struct({ error: Schema.String });

const TERMINAL_STATUSES: ReadonlyArray<TaskStatus> = [
  'completed',
  'failed',
  'cancelled',
];

export function isTerminal(status: TaskStatus): boolean {
  return TERMINAL_STATUSES.includes(status);
}

export function isActive(status: TaskStatus): boolean {
  return status === 'submitted' || status === 'running';
}
