'use strict';

const TERMINAL_STATUSES = new Set(['completed', 'failed', 'cancelled']);
const STATUS_LABELS = {
  submitted: 'Submitted',
  running: 'Running',
  completed: 'Completed',
  failed: 'Failed',
  cancelled: 'Cancelled'
};
const KIND_LABELS = {
  message: 'Message',
  thought: 'Thought',
  tool_call: 'Tool call',
  permission: 'Permission'
};
const LIVE_UPDATES_OFF = 'live updates are off; polling';

const errorBanner = document.getElementById('error-banner');
const errorMessage = document.getElementById('error-message');
const errorDismiss = document.getElementById('error-dismiss');
const submitForm = document.getElementById('submit-form');
const titleInput = document.getElementById('task-title');
const instructionInput = document.getElementById('task-instruction');
const agentSelect = document.getElementById('task-agent');
const hostInput = document.getElementById('task-host');
const hostOptions = document.getElementById('host-options');
const workspaceInput = document.getElementById('task-workspace');
const submitButton = document.getElementById('submit-button');
const taskList = document.getElementById('task-list');
const taskListEmpty = document.getElementById('task-list-empty');
const detailEmpty = document.getElementById('detail-empty');
const detail = document.getElementById('detail');
const detailTitle = document.getElementById('detail-title');
const detailStatus = document.getElementById('detail-status');
const detailAgent = document.getElementById('detail-agent');
const detailHost = document.getElementById('detail-host');
const detailWorkspace = document.getElementById('detail-workspace');
const detailOutcome = document.getElementById('detail-outcome');
const cancelButton = document.getElementById('cancel-button');
const cancelStatus = document.getElementById('cancel-status');
const transcript = document.getElementById('transcript');
const transcriptEmpty = document.getElementById('transcript-empty');

const state = {
  tasks: [],
  selectedId: null,
  detail: null,
  submitting: false,
  cancelling: false,
  cancelRequestedId: null,
  listSignature: '',
  transcriptKey: '',
  lastTranscript: null,
  // The banner text this page wrote for a journal failure, so recovery hides only that.
  journalErrorText: null,
  listSeq: 0,
  detailSeq: 0
};

let eventSource = null;
let journalSource = null;
let refreshTimer = null;

function textOr(value, fallback) {
  return typeof value === 'string' && value !== '' ? value : fallback;
}

function setText(element, value) {
  if (element.textContent !== value) element.textContent = value;
}

function normalizedStatus(value) {
  return typeof value === 'string' ? value.toLowerCase() : '';
}

function statusLabel(status) {
  if (Object.hasOwn(STATUS_LABELS, status)) return STATUS_LABELS[status];
  if (status === '') return 'Unknown';
  return status.charAt(0).toUpperCase() + status.slice(1);
}

function badgeClass(status) {
  return /^[a-z]+$/.test(status) ? 'badge-' + status : 'badge-unknown';
}

function kindLabel(kind) {
  if (Object.hasOwn(KIND_LABELS, kind)) return KIND_LABELS[kind];
  if (kind === '' || kind === 'unknown') return 'Update';
  return kind.charAt(0).toUpperCase() + kind.slice(1);
}

function compareTasks(a, b) {
  if (a.id === b.id) return 0;
  return a.id < b.id ? 1 : -1;
}

class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

async function api(path, options) {
  const init = Object.assign({}, options);
  const headers = Object.assign({ Accept: 'application/json' }, init.headers || {});
  if (init.body !== undefined && init.body !== null) headers['Content-Type'] = 'application/json';
  init.headers = headers;

  let response;
  try {
    response = await fetch(path, init);
  } catch {
    throw new ApiError(0, 'Could not reach the control plane.');
  }

  if (!response.ok) {
    let message = response.statusText || 'Request failed';
    try {
      const body = await response.json();
      if (body && typeof body.error === 'string' && body.error !== '') message = body.error;
    } catch {
      // The error body was not JSON; the status text stands in for it.
    }
    throw new ApiError(response.status, message);
  }

  const text = await response.text();
  if (text === '') return null;
  try {
    return JSON.parse(text);
  } catch {
    throw new ApiError(response.status, 'The server returned a response that is not JSON.');
  }
}

function showError(error) {
  const status = error && typeof error.status === 'number' ? error.status : 0;
  const message = error && typeof error.message === 'string' && error.message !== ''
    ? error.message
    : 'The request failed.';

  let text;
  if (status === 401) {
    text = 'HTTP 401 (' + message + '). This deployment requires a bearer token, which this page cannot supply.';
  } else if (status > 0) {
    text = 'HTTP ' + status + ' (' + message + ').';
  } else {
    text = message;
  }

  setText(errorMessage, text);
  errorBanner.hidden = false;
  return text;
}

function hideError() {
  errorBanner.hidden = true;
  setText(errorMessage, '');
}

// A journal failure is this page's to clear only while the banner still shows its text,
// so another request's message is never dismissed on its behalf.
function clearJournalError() {
  if (state.journalErrorText !== null
    && !errorBanner.hidden
    && errorMessage.textContent === state.journalErrorText) {
    hideError();
  }
  state.journalErrorText = null;
}

function buildTaskRow(task) {
  const item = document.createElement('li');
  item.className = 'task-item';

  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'task-row';
  button.dataset.id = task.id;
  if (task.id === state.selectedId) button.setAttribute('aria-current', 'true');
  button.addEventListener('click', () => {
    selectTask(task.id);
  });

  const status = normalizedStatus(task.status);
  const top = document.createElement('span');
  top.className = 'task-row-top';

  const badge = document.createElement('span');
  badge.className = 'badge ' + badgeClass(status);
  badge.textContent = statusLabel(status);
  top.appendChild(badge);

  const title = document.createElement('span');
  title.className = 'task-title';
  title.textContent = textOr(task.title, 'Untitled task');
  top.appendChild(title);

  const meta = document.createElement('span');
  meta.className = 'task-meta';

  const agent = document.createElement('span');
  agent.className = 'task-agent';
  agent.textContent = 'agent: ' + textOr(task.agent, 'unknown');
  meta.appendChild(agent);

  const host = document.createElement('span');
  host.className = 'task-host';
  host.textContent = 'host: ' + textOr(task.host, 'agent default');
  meta.appendChild(host);

  if (Number.isInteger(task.updates)) {
    const updates = document.createElement('span');
    updates.className = 'task-updates';
    updates.textContent = task.updates === 1 ? '1 update' : String(task.updates) + ' updates';
    meta.appendChild(updates);
  }

  button.appendChild(top);
  button.appendChild(meta);
  item.appendChild(button);
  return item;
}

function renderList() {
  const signature = state.selectedId + '|' + JSON.stringify(state.tasks);
  if (signature === state.listSignature) return;
  state.listSignature = signature;

  const active = document.activeElement;
  const focusId = active && active.classList && active.classList.contains('task-row')
    ? active.dataset.id
    : null;
  const scrollTop = taskList.scrollTop;

  taskList.replaceChildren();
  if (state.tasks.length === 0) {
    taskList.hidden = true;
    taskListEmpty.hidden = false;
  } else {
    taskList.hidden = false;
    taskListEmpty.hidden = true;
    const fragment = document.createDocumentFragment();
    for (const task of state.tasks) fragment.appendChild(buildTaskRow(task));
    taskList.appendChild(fragment);
  }

  taskList.scrollTop = scrollTop;
  if (focusId !== null) {
    for (const row of taskList.querySelectorAll('.task-row')) {
      if (row.dataset.id === focusId) {
        row.focus();
        break;
      }
    }
  }
}

function buildEntry(value) {
  const update = value && typeof value === 'object' ? value : {};
  const rawKind = typeof update.kind === 'string' ? update.kind : '';
  const kind = /^[a-z_]+$/.test(rawKind) ? rawKind : 'unknown';

  const entry = document.createElement('article');
  entry.className = 'entry entry-' + kind;

  const head = document.createElement('div');
  head.className = 'entry-head';

  const label = document.createElement('span');
  label.className = 'entry-kind';
  label.textContent = kindLabel(kind);
  head.appendChild(label);

  const hasStatus = kind === 'tool_call' || kind === 'permission';
  if (hasStatus) {
    const status = document.createElement('span');
    status.className = 'entry-status';
    status.textContent = textOr(update.status, 'unknown');
    head.appendChild(status);
  }
  entry.appendChild(head);

  if (hasStatus) {
    const title = document.createElement('p');
    title.className = 'entry-title';
    title.textContent = textOr(
      update.title,
      kind === 'permission' ? 'Untitled permission' : 'Untitled tool call'
    );
    entry.appendChild(title);
  }

  const text = typeof update.text === 'string' ? update.text : '';
  if (text !== '') {
    const body = document.createElement('p');
    body.className = 'entry-text';
    body.textContent = text;
    entry.appendChild(body);
  }

  return entry;
}

function renderTranscript(updates, total) {
  const atBottom = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 32;
  const previousTop = transcript.scrollTop;

  transcript.replaceChildren();
  if (updates.length > 0 && total > updates.length) {
    const note = document.createElement('p');
    note.className = 'transcript-note';
    note.textContent = 'Showing the most recent ' + updates.length + ' of ' + total + ' updates.';
    transcript.appendChild(note);
  }
  for (const update of updates) transcript.appendChild(buildEntry(update));

  if (updates.length === 0) {
    transcript.hidden = true;
    transcriptEmpty.hidden = false;
    return;
  }

  transcript.hidden = false;
  transcriptEmpty.hidden = true;
  transcript.scrollTop = atBottom ? transcript.scrollHeight : previousTop;
}

function updateCancelButton() {
  const status = state.detail ? normalizedStatus(state.detail.status) : '';
  const requested = state.cancelRequestedId !== null && state.cancelRequestedId === state.selectedId;
  cancelButton.disabled = state.selectedId === null
    || TERMINAL_STATUSES.has(status)
    || state.cancelling
    || requested;
  setText(cancelStatus, state.cancelling ? 'Cancelling...' : '');
}

function renderDetail(payload, entries, total) {
  const taskState = payload && typeof payload.state === 'object' && payload.state !== null
    ? payload.state
    : {};
  const spec = typeof taskState.spec === 'object' && taskState.spec !== null ? taskState.spec : {};
  const status = normalizedStatus(taskState.status);
  const updates = Array.isArray(entries) ? entries : [];
  const transcriptTotal = Number.isInteger(total) ? total : updates.length;

  state.detail = taskState;
  if (TERMINAL_STATUSES.has(status)) state.cancelRequestedId = null;

  setText(detailTitle, textOr(spec.title, 'Untitled task'));
  detailStatus.className = 'badge ' + badgeClass(status);
  setText(detailStatus, statusLabel(status));
  setText(detailAgent, textOr(spec.agent, 'unknown'));
  setText(detailHost, textOr(spec.host, 'agent default'));
  setText(detailWorkspace, textOr(spec.workspace, 'none'));
  setText(detailOutcome, textOr(taskState.outcome, 'None yet.'));

  const transcriptKey = [state.selectedId, transcriptTotal, updates.length].join('|');
  if (transcriptKey !== state.transcriptKey) {
    state.transcriptKey = transcriptKey;
    renderTranscript(updates, transcriptTotal);
  }

  detailEmpty.hidden = true;
  detail.hidden = false;
  updateCancelButton();
}

function renderDetailEmpty() {
  state.detail = null;
  state.transcriptKey = '';
  detail.hidden = true;
  detailEmpty.hidden = false;
  setText(detailEmpty, 'Select a task to see its transcript.');
  updateCancelButton();
}

async function refreshList() {
  const seq = ++state.listSeq;
  const data = await api('/api/tasks');
  if (seq !== state.listSeq) return;

  const tasks = data && Array.isArray(data.tasks) ? data.tasks : [];
  state.tasks = tasks
    .filter((task) => task && typeof task === 'object' && typeof task.id === 'string' && task.id !== '')
    .sort(compareTasks);
  renderList();
}

async function refreshSelected() {
  const id = state.selectedId;
  if (id === null) return;

  const seq = ++state.detailSeq;
  // The page renders the status from the domain state, so asking for no events keeps a
  // long domain log off the wire; the transcript comes from the journal endpoint.
  const [detail, journal] = await Promise.allSettled([
    api('/api/tasks/' + encodeURIComponent(id) + '?events=none'),
    api('/api/tasks/' + encodeURIComponent(id) + '/journal?limit=200')
  ]);

  if (detail.status === 'rejected') {
    const error = detail.reason;
    if (error && error.status === 404) {
      if (seq === state.detailSeq && state.selectedId === id) {
        state.selectedId = null;
        state.lastTranscript = null;
        clearJournalError();
        renderDetailEmpty();
        renderList();
        refreshList().catch(showError);
      }
      return;
    }
    throw error;
  }

  if (seq !== state.detailSeq || state.selectedId !== id) return;

  // A journal failure keeps the last transcript this page rendered for the task, so a
  // transient 500 does not look like data loss; a first failure shows the empty
  // transcript and the banner.
  let entries = [];
  let total = 0;
  let failure = null;
  if (journal.status === 'fulfilled' && journal.value && Array.isArray(journal.value.entries)) {
    entries = journal.value.entries;
    total = Number.isInteger(journal.value.total) ? journal.value.total : entries.length;
    state.lastTranscript = { entries: entries, total: total };
    clearJournalError();
  } else {
    failure = journal.reason || new Error('The journal could not be read.');
    if (state.lastTranscript !== null) {
      entries = state.lastTranscript.entries;
      total = state.lastTranscript.total;
    }
  }
  renderDetail(detail.value, entries, total);
  // The stored text stands in for "already reported", so a dismissed failure stays
  // dismissed on the next poll, while a failure that first appears under another
  // banner stores nothing and still surfaces once that banner is gone.
  if (failure !== null && state.journalErrorText === null && errorBanner.hidden) {
    state.journalErrorText = showError(failure);
  }
}

async function refresh() {
  const results = await Promise.allSettled([refreshList(), refreshSelected()]);
  for (const result of results) {
    if (result.status === 'rejected') showError(result.reason);
  }
}

async function selectTask(id) {
  const nextId = typeof id === 'string' && id !== '' ? id : null;
  if (nextId === state.selectedId) {
    if (nextId !== null) await refreshSelected();
    return;
  }

  state.selectedId = nextId;
  state.detail = null;
  state.transcriptKey = '';
  state.lastTranscript = null;
  clearJournalError();
  state.cancelRequestedId = null;
  transcript.replaceChildren();
  transcript.hidden = true;
  transcriptEmpty.hidden = true;
  renderList();

  if (nextId === null) {
    renderDetailEmpty();
    return;
  }

  detail.hidden = true;
  detailEmpty.hidden = false;
  setText(detailEmpty, 'Loading task...');

  try {
    await refreshSelected();
  } catch (error) {
    showError(error);
    setText(detailEmpty, 'Could not load this task. See the error above.');
  }
}

function updateSubmitButton() {
  submitButton.disabled = state.submitting;
  setText(submitButton, state.submitting ? 'Submitting...' : 'Delegate task');
}

async function loadAgents() {
  try {
    const data = await api('/api/agents');
    const agents = data && Array.isArray(data.agents)
      ? data.agents.filter((agent) => typeof agent === 'string' && agent !== '')
      : [];

    agentSelect.replaceChildren();
    if (agents.length === 0) {
      const option = document.createElement('option');
      option.value = '';
      option.textContent = 'No agents available';
      option.disabled = true;
      option.selected = true;
      agentSelect.appendChild(option);
      agentSelect.disabled = true;
      return;
    }

    for (const name of agents) {
      const option = document.createElement('option');
      option.value = name;
      option.textContent = name;
      agentSelect.appendChild(option);
    }
    const preferred = data && typeof data.default === 'string' ? data.default : '';
    agentSelect.value = agents.includes(preferred) ? preferred : agents[0];
    agentSelect.disabled = false;

    const hosts = data && Array.isArray(data.hosts)
      ? data.hosts.filter((host) => typeof host === 'string' && host !== '')
      : [];
    hostOptions.replaceChildren();
    for (const name of hosts) {
      const option = document.createElement('option');
      option.value = name;
      hostOptions.appendChild(option);
    }
  } catch (error) {
    agentSelect.replaceChildren();
    const option = document.createElement('option');
    option.value = '';
    option.textContent = 'Agents unavailable';
    option.disabled = true;
    option.selected = true;
    agentSelect.appendChild(option);
    agentSelect.disabled = true;
    throw error;
  }
}

async function handleSubmit(event) {
  event.preventDefault();
  if (state.submitting) return;

  const title = titleInput.value.trim();
  const instruction = instructionInput.value;
  const agent = agentSelect.value;

  if (title === '') {
    showError(new Error('Enter a title for the task.'));
    titleInput.focus();
    return;
  }
  if (instruction.trim() === '') {
    showError(new Error('Enter an instruction for the agent.'));
    instructionInput.focus();
    return;
  }
  if (agent === '') {
    showError(new Error('Select an agent.'));
    agentSelect.focus();
    return;
  }

  const host = hostInput.value.trim();
  const workspace = workspaceInput.value.trim();

  state.submitting = true;
  updateSubmitButton();
  try {
    const created = await api('/api/tasks', {
      method: 'POST',
      body: JSON.stringify({
        title: title,
        instruction: instruction,
        agent: agent,
        host: host === '' ? null : host,
        workspace: workspace === '' ? null : workspace
      })
    });

    titleInput.value = '';
    instructionInput.value = '';

    try {
      await refreshList();
    } catch (error) {
      showError(error);
    }

    if (created && typeof created.id === 'string' && created.id !== '') {
      await selectTask(created.id);
    }
  } catch (error) {
    showError(error);
  } finally {
    state.submitting = false;
    updateSubmitButton();
  }
}

async function cancelSelected() {
  const id = state.selectedId;
  if (id === null || state.cancelling) return;

  state.cancelling = true;
  updateCancelButton();
  try {
    await api('/api/tasks/' + encodeURIComponent(id) + '/cancel', {
      method: 'POST',
      body: JSON.stringify({ reason: 'Cancelled from the control plane.' })
    });
    state.cancelRequestedId = id;
    await refresh();
  } catch (error) {
    showError(error);
  } finally {
    state.cancelling = false;
    updateCancelButton();
  }
}

function scheduleRefresh() {
  if (refreshTimer !== null) return;
  refreshTimer = window.setTimeout(() => {
    refreshTimer = null;
    refresh().catch(showError);
  }, 200);
}

function connectEvents() {
  if (typeof EventSource !== 'function') return;
  eventSource = new EventSource('/api/events');
  journalSource = new EventSource('/api/journal/events');
  // EventSource reconnects by itself; a custom retry loop would duplicate it.
  for (const source of [eventSource, journalSource]) {
    source.addEventListener('message', () => {
      if (errorMessage.textContent === LIVE_UPDATES_OFF) hideError();
      scheduleRefresh();
    });
    // A token-protected feed answers 401 to EventSource, which cannot send the header.
    // Only fill an empty banner, so a live-feed notice never masks an API error.
    source.addEventListener('error', () => {
      if (errorBanner.hidden) showError(new Error(LIVE_UPDATES_OFF));
    });
  }
}

function startPolling() {
  window.setInterval(() => {
    refreshList().catch(showError);
  }, 5000);
  window.setInterval(() => {
    refreshSelected().catch(showError);
  }, 2000);
}

function bindEvents() {
  submitForm.addEventListener('submit', handleSubmit);
  cancelButton.addEventListener('click', cancelSelected);
  errorDismiss.addEventListener('click', hideError);
  window.addEventListener('pagehide', () => {
    if (eventSource !== null) eventSource.close();
    if (journalSource !== null) journalSource.close();
  });
}

async function init() {
  bindEvents();

  const results = await Promise.allSettled([loadAgents(), refreshList()]);
  for (const result of results) {
    if (result.status === 'rejected') showError(result.reason);
  }

  renderList();
  if (state.tasks.length > 0) {
    await selectTask(state.tasks[0].id);
  } else {
    renderDetailEmpty();
  }

  connectEvents();
  startPolling();
}

init().catch(showError);
