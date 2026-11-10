import { Badge } from '@cloudflare/kumo/components/badge';
import { Banner } from '@cloudflare/kumo/components/banner';
import { Button } from '@cloudflare/kumo/components/button';
import { Empty } from '@cloudflare/kumo/components/empty';
import { Sidebar } from '@cloudflare/kumo/components/sidebar';
import {
  ChatCircleIcon,
  ChatsCircleIcon,
  CopyIcon,
  FolderPlusIcon,
  GearSixIcon,
  MoonIcon,
  PlusIcon,
  SunIcon,
  XIcon,
} from '@phosphor-icons/react';
import {
  QueryClient,
  QueryClientProvider,
  useQueries,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query';
import { Effect } from 'effect';
import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';

import {
  createClient,
  normalizeBaseUrl,
  readConnection,
  readToken,
  writeBaseUrl,
  writeToken,
  type ApiClient,
  type ClientConfig,
} from './api/client';
import { ChatPane, ErrorBanner } from './components/ChatPane';
import {
  ConfirmRemoveDialog,
  SettingsDialog,
  WorkspaceDialog,
} from './components/Dialogs';
import { relativeTime } from './format';
import { applyTheme, readTheme, writeTheme, type ThemeMode } from './theme';
import {
  SAVED_WORKSPACES_KEY,
  NO_WORKSPACE_LABEL,
  decodeSavedWorkspaces,
  groupSessions,
  normalizeWorkspacePath,
  workspaceKey,
} from './domain/workspace';
import {
  isActive,
  type JournalResponse,
  type TaskStatus,
} from './domain/types';

const STATUS: Record<
  TaskStatus,
  {
    label: string;
    variant: 'neutral' | 'info' | 'success' | 'error' | 'warning';
  }
> = {
  new: { label: 'New', variant: 'neutral' },
  submitted: { label: 'Queued', variant: 'info' },
  running: { label: 'Running', variant: 'info' },
  completed: { label: 'Completed', variant: 'success' },
  failed: { label: 'Failed', variant: 'error' },
  cancelled: { label: 'Cancelled', variant: 'warning' },
};

function readSavedWorkspaces(): string[] {
  const raw = window.localStorage.getItem(SAVED_WORKSPACES_KEY);
  if (raw === null) {
    return [];
  }
  try {
    return decodeSavedWorkspaces(JSON.parse(raw)) ?? [];
  } catch {
    return [];
  }
}

function storeSavedWorkspaces(paths: ReadonlyArray<string>): void {
  window.localStorage.setItem(SAVED_WORKSPACES_KEY, JSON.stringify(paths));
}

export function App({ client }: { client?: ApiClient } = {}) {
  const [queryClient] = useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: { retry: false, refetchOnWindowFocus: false },
        },
      }),
  );
  return (
    <QueryClientProvider client={queryClient}>
      <AppShell injectedClient={client} />
    </QueryClientProvider>
  );
}

function AppShell({
  injectedClient,
}: {
  injectedClient: ApiClient | undefined;
}) {
  const queryClient = useQueryClient();
  const [initialConnection] = useState(() => {
    const connection = readConnection(window.localStorage);
    return {
      config: {
        baseUrl: connection.baseUrl,
        token: readToken(window.sessionStorage, connection.baseUrl),
      } satisfies ClientConfig,
      warning: connection.warning,
    };
  });
  const [config, setConfig] = useState<ClientConfig>(initialConnection.config);
  const [configWarning, setConfigWarning] = useState<string | null>(
    initialConnection.warning,
  );
  const [connectionRevision, setConnectionRevision] = useState(0);
  const client = useMemo(
    () => injectedClient ?? createClient(config),
    [injectedClient, config],
  );

  const [theme, setTheme] = useState<ThemeMode>(() =>
    readTheme(window.localStorage),
  );
  useEffect(() => {
    applyTheme(theme);
  }, [theme]);
  const toggleTheme = () => {
    const next: ThemeMode = theme === 'dark' ? 'light' : 'dark';
    setTheme(next);
    writeTheme(window.localStorage, next);
  };

  const [savedWorkspaces, setSavedWorkspaces] =
    useState<string[]>(readSavedWorkspaces);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(
    null,
  );
  const [liveTaskId, setLiveTaskId] = useState<string | null>(null);
  const [workspaceDialogOpen, setWorkspaceDialogOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [removePath, setRemovePath] = useState<string | null>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);

  const agentsQuery = useQuery({
    queryKey: ['agents', connectionRevision],
    queryFn: () => Effect.runPromise(client.fetchAgents()),
    retry: 1,
    refetchInterval: 30_000,
  });
  const tasksQuery = useQuery({
    queryKey: ['tasks', connectionRevision],
    queryFn: () => Effect.runPromise(client.fetchTasks()),
    refetchInterval: 2000,
  });

  const tasks = tasksQuery.data?.tasks ?? [];
  const groups = groupSessions(tasks, savedWorkspaces);
  const activeGroup =
    selectedKey === null
      ? groups[0]
      : (groups.find((group) => group.workspace.key === selectedKey) ??
        groups[0]);

  const workspaceTasks =
    activeGroup === undefined
      ? []
      : tasks.filter(
          (task) =>
            workspaceKey(task.workspace ?? null) === activeGroup.workspace.key,
        );

  const journalQueries = useQueries({
    queries: workspaceTasks.map((task) => ({
      queryKey: ['journal', connectionRevision, task.id],
      queryFn: () => Effect.runPromise(client.fetchJournal(task.id)),
      retry: 1,
      retryDelay: 500,
      refetchInterval:
        isActive(task.status) && task.id !== liveTaskId ? 1000 : false,
    })),
  });

  const journals: Record<string, JournalResponse | undefined> = {};
  const journalErrors: Record<string, string | undefined> = {};
  workspaceTasks.forEach((task, index) => {
    const query = journalQueries[index];
    journals[task.id] = query?.data;
    if (query?.error) {
      journalErrors[task.id] = query.error.message;
    }
  });
  const journalError =
    workspaceTasks
      .map((task) => journalErrors[task.id])
      .find((message) => message !== undefined) ?? null;

  const removeGroup =
    removePath === null
      ? undefined
      : groups.find(
          (group) => group.workspace.key === workspaceKey(removePath),
        );
  const removeHasSessions = (removeGroup?.sessions.length ?? 0) > 0;

  const selectedSession =
    selectedSessionId === null
      ? undefined
      : activeGroup?.sessions.find(
          (session) => session.id === selectedSessionId,
        );

  const focusComposer = useCallback(() => {
    if (typeof requestAnimationFrame === 'function') {
      requestAnimationFrame(() => {
        composerRef.current?.focus();
      });
    }
  }, []);

  const handleTaskCreated = useCallback(
    (id: string) => {
      setSelectedSessionId(id);
      setLiveTaskId(id);
      void queryClient.invalidateQueries({
        queryKey: ['tasks', connectionRevision],
      });
      focusComposer();
    },
    [focusComposer, queryClient, connectionRevision],
  );

  const handleRunFinished = useCallback(
    (id: string) => {
      setLiveTaskId((current) => (current === id ? null : current));
      void queryClient.invalidateQueries({
        queryKey: ['tasks', connectionRevision],
      });
      void queryClient.invalidateQueries({
        queryKey: ['journal', connectionRevision, id],
      });
    },
    [queryClient, connectionRevision],
  );

  const retryJournal = useCallback(
    (id: string) => {
      void queryClient.invalidateQueries({
        queryKey: ['journal', connectionRevision, id],
      });
    },
    [queryClient, connectionRevision],
  );

  const addWorkspace = (path: string) => {
    const normalized = normalizeWorkspacePath(path);
    if (!savedWorkspaces.some((saved) => workspaceKey(saved) === normalized)) {
      const next = [...savedWorkspaces, normalized];
      setSavedWorkspaces(next);
      storeSavedWorkspaces(next);
    }
    setSelectedKey(normalized);
    setSelectedSessionId(null);
  };

  const removeWorkspace = () => {
    if (removePath === null) {
      return;
    }
    const key = workspaceKey(removePath);
    const group = groups.find((candidate) => candidate.workspace.key === key);
    const hadSessions = (group?.sessions.length ?? 0) > 0;
    const next = savedWorkspaces.filter((saved) => workspaceKey(saved) !== key);
    setSavedWorkspaces(next);
    storeSavedWorkspaces(next);
    if (!hadSessions && workspaceKey(selectedKey) === key) {
      setSelectedKey(null);
      setSelectedSessionId(null);
      setLiveTaskId(null);
    }
    setRemovePath(null);
  };

  const startSession = () => {
    if (activeGroup === undefined) {
      setWorkspaceDialogOpen(true);
      return;
    }
    setSelectedKey(activeGroup.workspace.key);
    setSelectedSessionId(null);
    focusComposer();
  };

  const saveConnection = (next: ClientConfig) => {
    const baseUrl = normalizeBaseUrl(next.baseUrl);
    const token = next.token;
    if (baseUrl === config.baseUrl && token === config.token) {
      return;
    }
    setConfig({ baseUrl, token });
    writeBaseUrl(window.localStorage, baseUrl);
    writeToken(window.sessionStorage, token, baseUrl);
    setConfigWarning(null);
    setConnectionRevision((revision) => revision + 1);
    queryClient.clear();
  };

  const cancelSelected = () => {
    if (selectedSession === undefined) {
      return;
    }
    const id = selectedSession.id;
    void Effect.runPromise(client.cancelTask(id))
      .catch(() => {})
      .finally(() => handleRunFinished(id));
  };

  const copySessionId = () => {
    if (selectedSession !== undefined) {
      void navigator.clipboard?.writeText(selectedSession.id);
    }
  };

  const connection = agentsQuery.isSuccess
    ? 'ok'
    : agentsQuery.isError
      ? 'error'
      : 'loading';
  const agentName = agentsQuery.data?.default ?? 'no agent';
  const queryError =
    agentsQuery.error?.message ?? tasksQuery.error?.message ?? journalError;

  return (
    <div className="app-root">
      <a className="skip-link" href="#main">
        Skip to main content
      </a>
      <Sidebar.Provider
        defaultOpen
        collapsible="icon"
        resizable
        defaultWidth={288}
        minWidth={280}
        maxWidth={300}
        mobileBreakpoint={768}
        className="app-shell"
      >
        <Sidebar>
          <Sidebar.Header className="sidebar-header">
            <div className="brand">
              <span className="brand-mark" aria-hidden="true">
                m
              </span>
              <span className="brand-text">
                <span className="brand-name">mokmok</span>
                <span className="brand-subtitle">control plane</span>
              </span>
            </div>
            <div className="brand-actions">
              <Button
                size="sm"
                variant="primary"
                onClick={startSession}
                aria-label="New session"
              >
                <PlusIcon size={14} />
                New
              </Button>
              <Button
                shape="square"
                size="sm"
                variant="ghost"
                aria-label="Connection settings"
                onClick={() => setSettingsOpen(true)}
              >
                <GearSixIcon size={16} />
              </Button>
            </div>
          </Sidebar.Header>

          <Sidebar.Content>
            {groups.map((group) => {
              const saved = savedWorkspaces.some(
                (path) => workspaceKey(path) === group.workspace.key,
              );
              return (
                <Sidebar.Group key={group.workspace.key || 'no-workspace'}>
                  <Sidebar.GroupLabel
                    className="sidebar-group-label"
                    title={group.workspace.path ?? NO_WORKSPACE_LABEL}
                  >
                    <span className="group-label-text">
                      {group.workspace.label}
                    </span>
                    <span className="group-count">{group.sessions.length}</span>
                    {saved && group.workspace.path !== null ? (
                      <Button
                        shape="square"
                        size="xs"
                        variant="ghost"
                        className="group-remove"
                        aria-label={
                          group.sessions.length > 0
                            ? `Unpin workspace ${group.workspace.label}`
                            : `Remove workspace ${group.workspace.label}`
                        }
                        onClick={() => {
                          setRemovePath(group.workspace.path);
                        }}
                      >
                        <XIcon size={12} />
                      </Button>
                    ) : null}
                  </Sidebar.GroupLabel>
                  <Sidebar.Menu>
                    {group.sessions.map((session) => {
                      const active = session.id === selectedSessionId;
                      return (
                        <Sidebar.MenuButton
                          key={session.id}
                          icon={ChatCircleIcon}
                          active={active}
                          aria-current={active ? 'page' : undefined}
                          className="session-item"
                          onClick={() => {
                            setSelectedKey(group.workspace.key);
                            setSelectedSessionId(session.id);
                          }}
                        >
                          <span className="session-text">
                            <span className="session-title">
                              {session.title}
                            </span>
                            <span className="session-meta">
                              <span
                                className="status-dot"
                                data-status={
                                  journalErrors[session.id]
                                    ? 'error'
                                    : session.status
                                }
                                aria-hidden="true"
                              />
                              {relativeTime(session.createdAt)}
                            </span>
                          </span>
                        </Sidebar.MenuButton>
                      );
                    })}
                    {group.sessions.length === 0 ? (
                      <Sidebar.MenuButton
                        icon={PlusIcon}
                        onClick={() => {
                          setSelectedKey(group.workspace.key);
                          setSelectedSessionId(null);
                          focusComposer();
                        }}
                      >
                        Add session
                      </Sidebar.MenuButton>
                    ) : null}
                  </Sidebar.Menu>
                </Sidebar.Group>
              );
            })}
            <Sidebar.Menu>
              <Sidebar.MenuButton
                icon={FolderPlusIcon}
                onClick={() => setWorkspaceDialogOpen(true)}
              >
                Add workspace
              </Sidebar.MenuButton>
            </Sidebar.Menu>
          </Sidebar.Content>

          <Sidebar.Footer className="sidebar-footer">
            <div className="footer-status">
              <span
                className="status-dot"
                data-status={connection === 'ok' ? 'completed' : connection}
                aria-hidden="true"
              />
              <span>
                {connection === 'ok'
                  ? 'Connected'
                  : connection === 'error'
                    ? 'Unreachable'
                    : 'Connecting'}
              </span>
            </div>
            <div className="footer-actions">
              <span className="footer-agent" title={agentName}>
                {agentName}
              </span>
              <Button
                shape="square"
                size="sm"
                variant="ghost"
                aria-label={
                  theme === 'dark' ? 'Use light theme' : 'Use dark theme'
                }
                onClick={toggleTheme}
              >
                {theme === 'dark' ? (
                  <SunIcon size={16} />
                ) : (
                  <MoonIcon size={16} />
                )}
              </Button>
              <Sidebar.Trigger aria-label="Toggle sidebar" />
            </div>
          </Sidebar.Footer>
        </Sidebar>

        <main className="main-pane" id="main" tabIndex={-1}>
          <header className="main-header">
            <Sidebar.Trigger
              className="sidebar-trigger-mobile"
              aria-label="Toggle sidebar"
            />
            <div className="header-title">
              <span className="workspace-path">
                {activeGroup?.workspace.path ?? NO_WORKSPACE_LABEL}
              </span>
              <h1 className="session-heading">
                {selectedSession?.title ?? 'New session'}
              </h1>
            </div>
            <div className="header-actions">
              {selectedSession === undefined ? null : (
                <Fragment>
                  <Badge variant={STATUS[selectedSession.status].variant}>
                    {STATUS[selectedSession.status].label}
                  </Badge>
                  {isActive(selectedSession.status) ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={cancelSelected}
                    >
                      Cancel
                    </Button>
                  ) : null}
                  <Button
                    shape="square"
                    size="sm"
                    variant="ghost"
                    aria-label="Copy session id"
                    onClick={copySessionId}
                  >
                    <CopyIcon size={14} />
                  </Button>
                </Fragment>
              )}
            </div>
          </header>

          <ErrorBanner message={queryError} />
          {configWarning === null ? null : (
            <Banner
              variant="alert"
              title="Configuration warning"
              description={configWarning}
              className="error-banner"
            />
          )}

          {activeGroup === undefined ? (
            <div className="empty-state">
              <Empty
                icon={<ChatsCircleIcon size={40} />}
                title="No workspace selected"
                description="Add a workspace to delegate an instruction to an agent."
                contents={
                  <Button
                    variant="primary"
                    onClick={() => setWorkspaceDialogOpen(true)}
                  >
                    Add workspace
                  </Button>
                }
              />
            </div>
          ) : (
            <ChatPane
              key={`${activeGroup.workspace.key}\u0000${connectionRevision}`}
              workspace={activeGroup.workspace}
              tasks={workspaceTasks}
              journals={journals}
              journalErrors={journalErrors}
              agents={agentsQuery.data}
              client={client}
              selectedSessionId={selectedSessionId}
              onTaskCreated={handleTaskCreated}
              onRunFinished={handleRunFinished}
              onRetryJournal={retryJournal}
              composerRef={composerRef}
            />
          )}
        </main>
      </Sidebar.Provider>

      <WorkspaceDialog
        open={workspaceDialogOpen}
        onOpenChange={setWorkspaceDialogOpen}
        onAdd={addWorkspace}
      />
      <SettingsDialog
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        initial={config}
        initialError={configWarning}
        onSave={saveConnection}
      />
      <ConfirmRemoveDialog
        open={removePath !== null}
        path={removePath}
        unpin={removeHasSessions}
        onOpenChange={(open) => {
          if (!open) {
            setRemovePath(null);
          }
        }}
        onConfirm={removeWorkspace}
      />
    </div>
  );
}
