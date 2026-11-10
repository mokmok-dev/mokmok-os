import type { MessagePart, UIMessage } from '@tanstack/ai-client';
import { useChat } from '@tanstack/ai-react';
import { Banner } from '@cloudflare/kumo/components/banner';
import { Button } from '@cloudflare/kumo/components/button';
import { Empty } from '@cloudflare/kumo/components/empty';
import { Textarea } from '@cloudflare/kumo/components/input';
import { Select } from '@cloudflare/kumo/components/select';
import { ChatsCircleIcon } from '@phosphor-icons/react';
import { Effect } from 'effect';
import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type RefObject,
} from 'react';

import type { ApiClient } from '../api/client';
import { makeControlPlaneFetcher } from '../api/fetcher';
import { previewInstruction } from '../domain/session';
import { uiMessagesForWorkspace } from '../domain/transcript';
import type {
  AgentsResponse,
  JournalEntry,
  JournalResponse,
  TaskListEntry,
} from '../domain/types';
import type { Workspace } from '../domain/workspace';

export interface ChatPaneProps {
  workspace: Workspace;
  tasks: ReadonlyArray<TaskListEntry>;
  journals: Readonly<Record<string, JournalResponse | undefined>>;
  journalErrors: Readonly<Record<string, string | undefined>>;
  agents: AgentsResponse | undefined;
  client: ApiClient;
  selectedSessionId: string | null;
  onTaskCreated: (id: string) => void;
  onRunFinished: (id: string) => void;
  onRetryJournal: (id: string) => void;
  composerRef: RefObject<HTMLTextAreaElement | null>;
}

export function ErrorBanner({ message }: { message: string | null }) {
  const [dismissed, setDismissed] = useState<string | null>(null);
  if (message === null || message === dismissed) {
    return null;
  }
  return (
    <Banner
      variant="error"
      title="Control plane error"
      description={message}
      className="error-banner"
      action={
        <Button
          size="sm"
          variant="secondary"
          onClick={() => setDismissed(message)}
        >
          Dismiss
        </Button>
      }
    />
  );
}

function turnId(message: UIMessage): string {
  const taskId: unknown = message.metadata?.['taskId'];
  return typeof taskId === 'string' ? taskId : '';
}

function PartView({ part }: { part: MessagePart }) {
  if (part.type === 'text') {
    return <p className="text-part">{part.content}</p>;
  }
  if (part.type === 'tool-call') {
    return (
      <div className="tool-card">
        <div className="tool-card-head">
          <span className="tool-name">{part.name}</span>
          <span className="tool-state" data-state={part.state}>
            {part.state}
          </span>
        </div>
        <pre className="tool-args">{part.arguments}</pre>
        {part.output === undefined ? null : (
          <div className="tool-output">{String(part.output)}</div>
        )}
      </div>
    );
  }
  return null;
}

function MessageView({
  message,
  active,
}: {
  message: UIMessage;
  active: boolean;
}) {
  const isUser = message.role === 'user';
  const metadata = message.metadata ?? {};
  const status = metadata['status'];
  const outcome = metadata['outcome'];
  const showPreview =
    isUser &&
    message.parts.some(
      (part) =>
        part.type === 'text' && previewInstruction(part.content).truncated,
    );
  const showOutcome =
    !isUser &&
    typeof outcome === 'string' &&
    outcome.length > 0 &&
    (status === 'failed' || status === 'cancelled');
  return (
    <article
      className={`turn ${isUser ? 'turn-user' : 'turn-assistant'}`}
      data-selected={active}
      data-task={turnId(message)}
    >
      {isUser ? (
        <div className="bubble">
          {message.parts.map((part, index) =>
            part.type === 'text' ? (
              <p className="text-part" key={`${message.id}-${index}`}>
                {previewInstruction(part.content).text}
              </p>
            ) : null,
          )}
          {showPreview ? (
            <p className="muted preview-note">
              Instruction preview; the agent received the full text.
            </p>
          ) : null}
        </div>
      ) : (
        <div className="assistant-body">
          {message.parts.length === 0 ? (
            <p className="muted">No transcript yet.</p>
          ) : (
            message.parts.map((part, index) => (
              <PartView part={part} key={`${message.id}-${index}`} />
            ))
          )}
          {showOutcome ? <p className="outcome-note">{outcome}</p> : null}
        </div>
      )}
    </article>
  );
}

export function ChatPane({
  workspace,
  tasks,
  journals,
  journalErrors,
  agents,
  client,
  selectedSessionId,
  onTaskCreated,
  onRunFinished,
  onRetryJournal,
  composerRef,
}: ChatPaneProps) {
  const entries: Record<string, ReadonlyArray<JournalEntry> | undefined> = {};
  for (const task of tasks) {
    entries[task.id] = journals[task.id]?.entries ?? [];
  }

  const [initialMessages] = useState(() =>
    uiMessagesForWorkspace(tasks, entries),
  );
  const [draft, setDraft] = useState('');
  const [agent, setAgent] = useState(agents?.default ?? '');
  const [createdTaskId, setCreatedTaskId] = useState<string | null>(null);
  const agentRef = useRef(agent);
  const mountedRef = useRef(true);
  const stoppedRuns = useRef(new Set<string>());
  const liveIdRef = useRef<string | null>(null);
  const currentRunId = useRef<string | null>(null);

  useEffect(() => {
    agentRef.current = agent;
  }, [agent]);

  useLayoutEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      stoppedRuns.current.clear();
      const id = liveIdRef.current;
      liveIdRef.current = null;
      if (id !== null) {
        onRunFinished(id);
      }
    };
  }, [onRunFinished]);

  useEffect(() => {
    if (agent.length === 0 && agents !== undefined) {
      setAgent(agents.default);
    }
  }, [agent, agents]);

  const cancelAndSettle = (id: string) => {
    void Effect.runPromise(client.cancelTask(id))
      .catch(() => {})
      .finally(() => onRunFinished(id));
  };

  const fetcher = useMemo(
    () =>
      makeControlPlaneFetcher({
        client,
        workspace: workspace.path,
        get agent() {
          return agentRef.current;
        },
        onTaskCreated: (id, runId) => {
          if (stoppedRuns.current.has(runId)) {
            stoppedRuns.current.delete(runId);
            cancelAndSettle(id);
            return;
          }
          if (mountedRef.current) {
            liveIdRef.current = id;
            setCreatedTaskId(id);
            onTaskCreated(id);
          }
        },
        onFinished: (id) => {
          liveIdRef.current = null;
          setCreatedTaskId(null);
          onRunFinished(id);
        },
        onCreateFailed: (runId, instruction) => {
          stoppedRuns.current.delete(runId);
          if (currentRunId.current !== runId) {
            return;
          }
          liveIdRef.current = null;
          setCreatedTaskId(null);
          setDraft((current) => (current.length === 0 ? instruction : current));
        },
      }),
    [],
  );

  const chat = useChat({
    threadId: workspace.key,
    initialMessages,
    fetcher,
  });

  useLayoutEffect(() => {
    if (chat.runId !== null) {
      currentRunId.current = chat.runId;
    }
  }, [chat.runId]);

  const signature = tasks
    .map((task) => {
      const journal = journals[task.id];
      return `${task.id}:${task.status}:${task.outcome ?? ''}:${
        journal?.entries.length ?? 0
      }:${journal?.total ?? 0}`;
    })
    .join('|');
  const previousSignature = useRef(signature);
  const pendingHydration = useRef(false);

  useEffect(() => {
    if (signature === previousSignature.current && !pendingHydration.current) {
      return;
    }
    if (chat.isLoading) {
      pendingHydration.current = true;
      return;
    }
    previousSignature.current = signature;
    pendingHydration.current = false;
    chat.setMessages(uiMessagesForWorkspace(tasks, entries));
  }, [signature, chat, tasks, journals]);

  useEffect(() => {
    if (chat.error !== undefined) {
      const id = liveIdRef.current;
      liveIdRef.current = null;
      setCreatedTaskId(null);
      if (chat.runId !== null) {
        stoppedRuns.current.delete(chat.runId);
      }
      if (id !== null) {
        onRunFinished(id);
      }
    }
  }, [chat.error, chat.runId, onRunFinished]);

  const scrollRef = useRef<HTMLDivElement>(null);
  const nearBottom = useRef(true);

  useEffect(() => {
    const element = scrollRef.current;
    if (element === null || !nearBottom.current) {
      return;
    }
    if (typeof element.scrollTo === 'function') {
      element.scrollTo({ top: element.scrollHeight, behavior: 'smooth' });
    }
  }, [chat.messages, chat.status]);

  useEffect(() => {
    if (selectedSessionId === null) {
      return;
    }
    const element = scrollRef.current;
    const target = element?.querySelector(`[data-task="${selectedSessionId}"]`);
    if (target instanceof HTMLElement) {
      target.scrollIntoView({ block: 'start', behavior: 'smooth' });
    }
  }, [selectedSessionId, chat.messages.length]);

  const canSend =
    agent.length > 0 && !chat.isLoading && draft.trim().length > 0;

  const send = () => {
    if (!canSend) {
      return;
    }
    const text = draft;
    setDraft('');
    void chat.sendMessage(text).catch(() => {
      setDraft((current) => (current.length === 0 ? text : current));
    });
  };

  const stop = () => {
    if (createdTaskId !== null) {
      liveIdRef.current = null;
      setCreatedTaskId(null);
      cancelAndSettle(createdTaskId);
    } else if (chat.runId !== null) {
      stoppedRuns.current.add(chat.runId);
    }
    chat.stop();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      send();
    }
  };

  const selectedJournal =
    selectedSessionId === null ? undefined : journals[selectedSessionId];
  const truncated =
    selectedJournal !== undefined &&
    selectedJournal.total > selectedJournal.entries.length;
  const failedJournals = tasks.filter((task) => journalErrors[task.id]);

  return (
    <section className="chat-pane" aria-label={`Chat for ${workspace.label}`}>
      <ErrorBanner message={chat.error?.message ?? null} />
      {truncated ? (
        <p className="truncation-note">
          Showing the most recent {selectedJournal.entries.length} of{' '}
          {selectedJournal.total} updates.
        </p>
      ) : null}
      <div
        className="chat-scroll"
        ref={scrollRef}
        onScroll={() => {
          const element = scrollRef.current;
          if (element === null) {
            return;
          }
          nearBottom.current =
            element.scrollHeight - element.scrollTop - element.clientHeight <
            120;
        }}
      >
        <p className="thread-label">
          Workspace thread · {tasks.length} session
          {tasks.length === 1 ? '' : 's'}
        </p>
        {selectedSessionId === null && tasks.length > 0 ? (
          <p className="new-session-note" role="status">
            New session — the next message starts a separate task in this
            workspace.
          </p>
        ) : null}
        {failedJournals.map((task) => (
          <p className="journal-error" key={task.id}>
            <span>
              Transcript for {task.title} could not be loaded:{' '}
              {journalErrors[task.id]}
            </span>
            <Button
              size="xs"
              variant="secondary"
              onClick={() => onRetryJournal(task.id)}
            >
              Retry
            </Button>
          </p>
        ))}
        {chat.messages.length === 0 ? (
          <div className="empty-state">
            <Empty
              icon={<ChatsCircleIcon size={40} />}
              title="No sessions yet"
              description="Send an instruction to start a session in this workspace."
            />
          </div>
        ) : (
          <div className="turn-list">
            {chat.messages.map((message) => {
              const taskId = turnId(message);
              return (
                <MessageView
                  key={message.id}
                  message={message}
                  active={
                    taskId === selectedSessionId ||
                    (taskId === '' && createdTaskId !== null)
                  }
                />
              );
            })}
          </div>
        )}
      </div>

      <div className="composer">
        <div className="composer-inner">
          <label className="visually-hidden" htmlFor="composer-input">
            Message
          </label>
          <Textarea
            id="composer-input"
            ref={composerRef}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={onKeyDown}
            placeholder="Delegate an instruction..."
            rows={3}
            autoResize
          />
          <div className="composer-actions">
            <Select
              aria-label="Agent"
              size="sm"
              value={agent}
              onValueChange={(value) => {
                setAgent(value ?? '');
              }}
            >
              {(agents?.agents ?? []).map((name) => (
                <Select.Option key={name} value={name}>
                  {name}
                </Select.Option>
              ))}
            </Select>
            <span className="run-status" aria-live="polite">
              {chat.error === undefined ? chat.status : 'error'}
            </span>
            {chat.isLoading ? (
              <Button type="button" variant="secondary" onClick={stop}>
                Stop
              </Button>
            ) : (
              <Button
                type="button"
                variant="primary"
                onClick={send}
                disabled={!canSend}
              >
                Send
              </Button>
            )}
          </div>
          <p className="composer-hint">
            Enter sends, Shift+Enter adds a line. Each session delegates one
            task; the agent sees only this message, while earlier turns stay in
            this workspace for you.
          </p>
        </div>
      </div>
    </section>
  );
}
