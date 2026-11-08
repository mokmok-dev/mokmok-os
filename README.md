# mokmok-os

mokmok OS is an "Operating System" for AI productivity originally developed for use inside mokmok.

## Control plane

`mokmok-controld` serves a small HTTP API and a single-page UI that submits tasks to
agents and records every step as a CloudEvent stream.

A scripted smoke run needs no agent:

```
nix run .#default -- --scripted --data-dir /tmp/mokmok
```

Then open the printed address (by default `http://127.0.0.1:8787`).

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
  one JSONL file per task. One daemon per directory.
- `--bind ADDR` (default `127.0.0.1:8787`): the HTTP listener.
- `--agents FILE` or `--scripted`: exactly one is required.
- `--workspace-root PATH`: every task workspace must resolve under this directory.
  A task without a workspace fails; a workspace that is missing, not a directory, or
  outside the root fails before the agent starts. Without the flag an ACP task may use
  any existing directory, so pass it to confine agents; `--scripted` does not read a
  workspace at all.
- `--workers N` (default 2): worker threads.
- `--allowed-host NAME` (repeatable): another `Host` header value the server answers.
  Requests whose `Host` is not listed get `403`; loopback names and the bound address
  are always allowed.

`MOKMOK_CONTROL_TOKEN` turns on bearer auth for every `/api` route. An empty value is
refused at startup (exit 2) rather than disabling auth, and a non-UTF-8 value is refused
too. A `--bind` address that is not loopback requires the variable; a loopback bind may
omit it. With a token set, a browser's `EventSource` cannot send the header, so the live
feed answers `401` and the page falls back to polling and says so.
