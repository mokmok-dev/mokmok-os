# mokmok-os

mokmok OS is an "Operating System" for AI productivity originally developed for use inside mokmok.

## Control plane

`mokmok-controld` serves the HTTP API that submits tasks to agents. The browser UI is a
separate app under `apps/web`; the daemon serves no HTML. Each task is kept twice: a
small CloudEvent stream with the facts that decide its status, and a compact execution
journal with its transcript.

A scripted smoke run needs no agent:

```
nix run .#default -- --scripted --data-dir "$(mktemp -d)"
```

`GET /healthz` answers `ok`. The daemon serves the API only: `/`, `/app.js`, and
`/app.css` are `404`, so start the web UI below. The journal is unredacted agent output
(see below), so the data directory must stay private; a world-readable location such as
a shared `/tmp/mokmok` exposes every transcript.

## Web UI

The UI is a pnpm-workspace app at `apps/web` (React, Vite, Effect, TanStack Query and
TanStack AI, with Kumo components). Node 22 and pnpm 10 come from the dev shell:

```
nix develop
pnpm install
pnpm --filter @mokmok/web dev
```

Run the daemon beside it:

```
nix run .#default -- --scripted --data-dir "$(mktemp -d)"
```

Then open `http://127.0.0.1:5173`. Vite proxies `/api` to
`MOKMOK_CONTROL_URL` (default `http://127.0.0.1:8787`), both while developing and from
`vite preview`, so the browser talks to one origin.

For production, build the static bundle and serve `apps/web/dist` behind a reverse
proxy that forwards `/api` to the daemon on the same origin:

```
pnpm --filter @mokmok/web build
```

```nginx
server {
  listen 8080;
  root /srv/mokmok-web; # apps/web/dist

  location / {
    try_files $uri /index.html;
  }

  location /api/ {
    proxy_pass http://127.0.0.1:8787;
    proxy_set_header Host 127.0.0.1:8787;
  }
}
```

The proxy must pass a `Host` value the daemon allows. The loopback form above always
is; a proxy that forwards the public name instead needs `--allowed-host control.example`.
A `--bind` address that is not loopback requires `MOKMOK_CONTROL_TOKEN`; set the same
token in the UI's Connection settings.

The daemon has no CORS layer, so the UI and `/api` must share an origin, either directly
or through the proxy above. `VITE_CONTROL_PLANE_URL` is the build-time fallback for the
API origin when no runtime setting is stored; it must point at that same origin (or a
proxy), not at a separate daemon host. An invalid runtime or build-time URL is reported
in a configuration banner instead of being used silently. The runtime setting in
Connection settings wins over the build-time default.

A bearer token is kept in `sessionStorage` (`mokmok.control.token`) as
`{origin, token}` and sent as an `Authorization: Bearer` header; it is never written to
`localStorage`, a URL, or a log. A token whose origin does not match the loaded base URL
is ignored and cleared, and changing the origin in the dialog clears it on blur or Save,
so it is never sent to a new host. The base URL setting lives in `localStorage` under
`mokmok.connection.v1`, and saved workspaces under `mokmok.workspaces.v1`.

Control-plane requests use a request timeout, 30 s by default. A timed-out submission
may still have been accepted by the daemon, so check the sidebar before resending: the
UI's 2 s task poll is how the workspace reconciles the listing with the daemon.

The UI follows a Codex/Claude-style layout: a sidebar groups sessions by workspace, and
the chat pane streams the selected workspace's transcript. One session is one delegated
task. A follow-up message starts a new session in the same workspace, and the sidebar
keeps every turn of that workspace together. The agent receives only the new
instruction; earlier turns are workspace context for you, not a continuation of the
agent's conversation. A failed or cancelled session renders its outcome under the
assistant turn.

The listing row carries the id, title, instruction preview, status, workspace, and
outcome, so grouping and the user turn need no detail read. The instruction preview is
capped at 4096 bytes on a character boundary plus an ellipsis; the task detail endpoint
carries the full instruction, and the UI derives the preview and its note from the text
itself. The daemon also reports the agent, transport host, journal count, and
`instruction_truncated` for other clients, which the UI ignores. The UI reads one journal
per session in the visible workspace, polls it only while that session is submitted or
running and is not the task the live fetcher is streaming, and lets that fetcher be the
single journal poller for the live task. Finishing a run invalidates the persisted
journal. A failed transcript read keeps the session listed, shows a per-session error
with a retry action, and keeps the last successful data.

Checks for the app:

```
pnpm --filter @mokmok/web test
pnpm --filter @mokmok/web typecheck
pnpm --filter @mokmok/web build
pnpm --filter @mokmok/web mutate
```

The frontend lives in `apps/web`, and the Nix flake builds it as `packages.web` with the
same typecheck, test, and build steps. `checks.webMutants` runs the Stryker gate on
Linux. Mutation testing covers `src/domain`, `src/api`, `src/format.ts`, and
`src/theme.ts`; it uses `vitest.mutation.config.ts`, which runs the unit tests only and
takes about three minutes instead of replaying the app flows for every mutant. The
components and `App.tsx` are covered by the app-level flows in
`src/tests/app.test.tsx` instead.

To run real agents, pass a JSON table with `--agents FILE`:

```json
{
  "agents": {
    "local": { "transport": "stdio", "program": "/usr/bin/grok", "args": ["agent", "stdio"] },
    "remote": {
      "transport": "websocket",
      "url": "wss://agents.example/acp",
      "secret": "the server-key query value",
      "bearer": "the Authorization: Bearer value"
    }
  },
  "hosts": {
    "eu": { "transport": "websocket", "url": "wss://eu.example/acp", "bearer": "token" }
  },
  "default": "local"
}
```

`secret` becomes the `server-key` query parameter and `bearer` the `Authorization`
header of the opening handshake. A remote agent runs `grok agent serve` and prints the
`wss://` URL to use; `ws://` is accepted only for `127.0.0.1`, `localhost`, and `[::1]`.

`hosts` maps a task's `host` value to a transport override. A task with a `host` the
table does not name fails instead of falling back to the agent's own transport. The
names are listed in `/api/agents` and offered by the form.

## TypeScript plugins

A task can run a plugin the user wrote in TypeScript instead of delegating to an agent.
A plugin is one file whose default export is an async function:

```ts
import type { Plugin } from '@mokmok/plugin-sdk';

const plugin: Plugin = async (context) => {
  context.emit({ kind: 'message', text: `picked up ${context.task.title}` });
  context.emit({ kind: 'tool_call', title: 'read the workspace', status: 'completed' });
  return { kind: 'completed', summary: 'the workspace looks fine' };
};

export default plugin;
```

Node strips the type annotations, so a plugin needs no build step. `context.emit` takes
the four event kinds the UI renders (`message`, `thought`, `tool_call`, `permission`),
and `context.signal` aborts when a human cancels or the run times out. Return
`{ kind: 'completed', summary }` to succeed, `{ kind: 'cancelled' }` to stop early on
purpose, and throw to fail: the thrown value's message and stack become the task's
failure reason.

A plugin is an entry in the same agent table as the ACP agents, so `/api/agents` and the
UI's agent picker list it with no other change:

```json
{
  "agents": {
    "hello": { "transport": "plugin", "entry": "/srv/mokmok/plugins/hello.ts", "timeout_secs": 120 },
    "local": { "transport": "stdio", "program": "/usr/bin/grok", "args": ["agent", "stdio"] }
  },
  "default": "hello"
}
```

`entry` is the plugin's absolute path. `timeout_secs` is optional and defaults to 600.
A plugin runs as one `node` process per task, started from `--node` (default `node` on
`PATH`), with the task's workspace as its working directory. The daemon kills the process
at the timeout, and again five seconds after a cancel the plugin ignores, so a plugin
cannot hold a task open.

The types come from Rust. `packages/plugin-sdk/types.gen.d.ts` is generated by
`pnpm --filter @mokmok/plugin-sdk gen` from `crates/mokmok-plugin-protocol`, and
`checks.pluginSdk` regenerates it in CI and fails when the committed file differs. The
runtime wire, `crates/mokmok-plugin-protocol`, is the same set of types, so a plugin's
event reaches `GET /api/tasks/{id}/journal` in the shape the UI reads.

A plugin runs with the daemon's user and inherits its environment, like a configured
agent binary, minus `MOKMOK_CONTROL_TOKEN`. Install a plugin the way you install an
agent: deliberately, in a table the operator controls.

Checks for the SDK:

```
pnpm --filter @mokmok/plugin-sdk test
pnpm --filter @mokmok/plugin-sdk typecheck
pnpm --filter @mokmok/plugin-sdk gen
```

Flags:

- `--data-dir PATH` (required): the local data root. The journal always lives in
  `<data-dir>/journal`; with `--store file` the task streams live in `<data-dir>/tasks`,
  one JSONL file per task. One daemon per data directory.
- `--store file|s3` (default `file`): where each task's domain stream is kept.
- `--s3-endpoint URL` (required with `--store s3`): the S3-compatible base URL, such as
  `http://127.0.0.1:9000` for MinIO or `https://s3.eu-central-1.amazonaws.com`.
- `--s3-bucket NAME` (required with `--store s3`): the bucket that holds the streams.
  Passing it or `--s3-endpoint` with `--store file` is refused.
- `--s3-prefix PATH` (default `tasks`): the path inside the bucket that holds the
  stream objects.
- `--s3-region REGION` (default `us-east-1`): the region every request is signed for.
- `--s3-timeout-secs SECS` (default 30): the deadline for one S3 call. It must be at
  least 1.
- `--bind ADDR` (default `127.0.0.1:8787`): the HTTP listener.
- `--agents FILE` or `--scripted`: exactly one is required.
- `--workspace-root PATH`: every task workspace must resolve under this directory.
  A task without a workspace fails; a workspace that is missing, not a directory, or
  outside the root fails before the agent starts. Without the flag an ACP task may use
  any existing directory, so pass it to confine agents; `--scripted` does not read a
  workspace at all.
- `--workers N` (default 2): worker threads.
- `--journal-max-bytes BYTES` (default 4194304): the size at which a task's active
  journal segment rotates. It must be at least 1.
- `--journal-segments N` (default 8): how many newest journal segments a task keeps,
  counting the active one. It must be at least 1.
- `--journal-max-age-secs SECS` (default 604800): how old a rotated journal segment
  may be before it is dropped. `0` keeps every rotated segment up to
  `--journal-segments`.
- `--allowed-host NAME` (repeatable): another `Host` header value the server answers.
  Requests whose `Host` is not listed get `403`; loopback names and the bound address
  are always allowed.
- `--node PATH` (default `node`): the Node executable TypeScript plugins run on. A table
  with no `plugin` entry never starts it.

With `--store s3` a task's domain stream is the object
`s3://<bucket>/<prefix>/<task-id>.jsonl`, holding the same JSONL records the file store
writes. Credentials come from `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and the
optional `AWS_SESSION_TOKEN`; a missing variable stops the daemon at startup and names
the variable. The execution journal stays on local disk under `<data-dir>/journal` in
both modes.

The daemon refuses to start against a store that does not honor `If-Match` and
`If-None-Match`. Opening the store writes a throwaway probe object and requires the
store to refuse an `If-Match` it cannot satisfy, because a store that accepts it would
silently drop a concurrent append instead of reporting a conflict. One daemon per
prefix is the supported deployment: the conditional write turns a racing append into a
conflict, but two daemons that each retry keep colliding instead of coordinating.
Listing tasks over S3 costs one list request per page plus one object read per task,
since a summary is replayed from its stream.

## Domain events and the execution journal

A task is stored twice, with different lifetimes:

- `<data-dir>/tasks/<id>.jsonl` with `--store file`, or
  `s3://<bucket>/<prefix>/<id>.jsonl` with `--store s3`, is the domain CloudEvent
  stream: `Submitted`, `Started`, `SessionOpened`, `Completed`, `Failed`, `Cancelled`,
  and bounded facts such as the completed summary. It is the source of truth for the
  task's state, so replay and listing stay fast however long the transcript grows.
- `<data-dir>/journal/<id>/<generation>.jsonl` is the execution journal: one compact
  JSON object per line with `seq`, `time`, and a `message`, `thought`, `tool_call`, or
  `permission` record. It is operational telemetry, is never replayed into the domain
  state, and is bounded per task. Every string field of a record is clipped to 64 KiB,
  so one huge chunk or tool title cannot escape that bound. Losing an old segment loses
  display history, not the task's business state. The journal is unredacted agent
  output, including tool titles that may carry a command line, so it must live in a
  private directory: on Unix the journal root, task directories, and segments are
  created with `0700`/`0600`, and an existing root keeps the mode an operator chose.

`GET /api/tasks/{id}` returns the domain state and a bounded domain-event tail, which
with the split is a handful of events. `GET /api/tasks/{id}/journal?limit=N` returns
the newest journal entries (default 200, at most 1000) with the full count, and
`GET /api/tasks` reports the journal entry count for each task. The UI renders the
transcript from the journal and the status and outcome from the domain state. The
count is cached after the first read, which the startup sweep warms; a count that
fails is not cached, so the next request tries again and the failure is logged at
debug level rather than once per poll.

`/api/events` streams domain CloudEvents and `/api/journal/events` streams
`{"task":"<uuid>","entry":{…}}` lines. Both feeds carry every task's records; neither
filters by task. Both require the same token and end when the daemon shuts down.

The journal rotates a task's active segment before a write would take it past
`--journal-max-bytes`, keeps at most `--journal-segments` newest segments, and drops
a rotated segment older than `--journal-max-age-secs`. The active segment is always
kept. Retention runs on an append and once when the journal opens, so a finished
task's expired segments are reclaimed too.

A task logged before the split still replays: the domain store keeps reading
`UpdateRecorded` events, and the journal endpoint projects them into the journal's
entry shape when a task has no journal of its own. The projection is read-only, and
new tasks never write `UpdateRecorded`. Once the journal has entries it wins: a task
with both `UpdateRecorded` events and a journal shows only the journal, and the
projection applies only when the journal is empty.

`MOKMOK_CONTROL_TOKEN` turns on bearer auth for every `/api` route. An empty value is
refused at startup (exit 2) rather than disabling auth, and a non-UTF-8 value is refused
too. A `--bind` address that is not loopback requires the variable; a loopback bind may
omit it. With a token set, a browser's `EventSource` cannot send the header, so the live
feeds answer `401`; the web UI polls the task and journal endpoints instead.
