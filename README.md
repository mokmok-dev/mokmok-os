# mokmok-os

mokmok OS is an "Operating System" for AI productivity originally developed for use inside mokmok.

## Control plane

`mokmok-controld` serves a small HTTP API and a single-page UI that submits tasks to
agents. Each task is kept twice: a small CloudEvent stream with the facts that decide
its status, and a compact execution journal with its transcript.

A scripted smoke run needs no agent:

```
nix run .#default -- --scripted --data-dir "$(mktemp -d)"
```

Then open the printed address (by default `http://127.0.0.1:8787`). The journal is
unredacted agent output (see below), so the data directory must stay private; a
world-readable location such as a shared `/tmp/mokmok` exposes every transcript.

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

Flags:

- `--data-dir PATH` (required): the store root; task streams live in `<data-dir>/tasks`,
  one JSONL file per task, and their journals in `<data-dir>/journal`. One daemon per
  directory.
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

## Domain events and the execution journal

A task is stored twice, with different lifetimes:

- `<data-dir>/tasks/<id>.jsonl` is the domain CloudEvent stream: `Submitted`,
  `Started`, `SessionOpened`, `Completed`, `Failed`, `Cancelled`, and bounded facts
  such as the completed summary. It is the source of truth for the task's state, so
  replay and listing stay fast however long the transcript grows.
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
feed answers `401` and the page falls back to polling and says so.
