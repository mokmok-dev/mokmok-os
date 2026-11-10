import type { ChatFetcher, MessagePart, UIMessage } from '@tanstack/ai-client';
import { Effect } from 'effect';

import {
  runError,
  runFinished,
  runStarted,
  textContent,
  textEnd,
  textStart,
  toolChunks,
} from '../domain/chunks';
import { titleFromInstruction } from '../domain/session';
import { isTerminal } from '../domain/types';
import {
  ApiError,
  JOURNAL_LIMIT,
  REQUEST_TIMEOUT_MS,
  type ApiClient,
} from './client';

export const POLL_INTERVAL_MS = 500;
export const MAX_POLL_FAILURES = 3;

export function timeoutMessage(): string {
  return `The submission timed out after ${
    REQUEST_TIMEOUT_MS / 1000
  } s. It may have been accepted; check the workspace before resending.`;
}

export interface ControlPlaneFetcherOptions {
  client: ApiClient;
  workspace: string | null;
  agent: string;
  onTaskCreated?: (taskId: string, runId: string) => void;
  onFinished?: (taskId: string) => void;
  onCreateFailed?: (runId: string, instruction: string) => void;
  maxPollFailures?: number;
}

export function lastUserText(messages: ReadonlyArray<UIMessage>): string {
  for (const message of messages.toReversed()) {
    if (message.role !== 'user') {
      continue;
    }
    return message.parts
      .filter(
        (part): part is Extract<MessagePart, { type: 'text' }> =>
          part.type === 'text',
      )
      .map((part) => part.content)
      .join('\n');
  }
  return '';
}

export function errorText(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  return 'the control plane request failed';
}

export function delay(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    let timer: ReturnType<typeof setTimeout>;
    const stop = () => {
      clearTimeout(timer);
      signal.removeEventListener('abort', stop);
      resolve();
    };
    timer = setTimeout(() => {
      signal.removeEventListener('abort', stop);
      resolve();
    }, ms);
    signal.addEventListener('abort', stop, { once: true });
    if (signal.aborted) {
      stop();
    }
  });
}

export function makeControlPlaneFetcher(
  options: ControlPlaneFetcherOptions,
): ChatFetcher {
  return async function* controlPlaneFetcher(input, { signal }) {
    if (signal.aborted) {
      return;
    }
    const instruction = lastUserText(input.messages);
    if (instruction.trim().length === 0) {
      yield runError('the message has no text', 'empty-message');
      return;
    }

    let id: string;
    try {
      const created = await Effect.runPromise(
        options.client.createTask({
          title: titleFromInstruction(instruction),
          instruction,
          agent: options.agent,
          ...(options.workspace === null
            ? {}
            : { workspace: options.workspace }),
        }),
      );
      id = created.id;
    } catch (error) {
      options.onCreateFailed?.(input.runId, instruction);
      yield runError(
        error instanceof ApiError && error.code === 'timeout'
          ? timeoutMessage()
          : errorText(error),
        'create-failed',
      );
      return;
    }

    options.onTaskCreated?.(id, input.runId);
    if (signal.aborted) {
      return;
    }
    yield runStarted(input.threadId, input.runId, Date.now());

    const seen = new Set<number>();
    const maxFailures = options.maxPollFailures ?? MAX_POLL_FAILURES;
    let failures = 0;
    let messageStarted = false;

    for (;;) {
      if (signal.aborted) {
        return;
      }
      try {
        const journal = await Effect.runPromise(
          options.client.fetchJournal(id, JOURNAL_LIMIT),
        );
        const detail = await Effect.runPromise(options.client.fetchTask(id));
        failures = 0;
        for (const entry of journal.entries) {
          if (seen.has(entry.seq)) {
            continue;
          }
          seen.add(entry.seq);
          if (!messageStarted) {
            yield textStart(id);
            messageStarted = true;
          }
          if (entry.kind === 'message' || entry.kind === 'thought') {
            yield textContent(id, entry.text);
          } else {
            yield* toolChunks(id, entry);
          }
        }
        const status = detail.state.status;
        if (isTerminal(status)) {
          if (messageStarted) {
            yield textEnd(id);
          }
          yield runFinished(input.threadId, input.runId, {
            taskId: id,
            status,
            outcome: detail.state.outcome ?? null,
          });
          options.onFinished?.(id);
          return;
        }
      } catch (error) {
        failures += 1;
        if (failures >= maxFailures) {
          yield runError(errorText(error), 'poll-failed');
          return;
        }
      }
      await delay(POLL_INTERVAL_MS, signal);
    }
  };
}
