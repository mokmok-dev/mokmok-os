# `@mokmok-os/agent`

One Durable Object, `WorkflowRunner`, is master of one workflow run. It owns the plan
and the journal, applies every transition through `@mokmok-os/workflow-core`'s
`reduce`, and persists to SQLite-backed storage. Model code reaches the run only
through the `workflow` Code Mode connector.

## Routes

Each route answers `{ ok: true, ... }` or `{ ok: false, error }`.

| Route            | Body                                           | Effect                                         |
| ---------------- | ---------------------------------------------- | ---------------------------------------------- |
| `GET /state`     | none                                           | `{ run }`, or `null` before a plan begins      |
| `POST /begin`    | `{ id, version, steps: [{ name, approval }] }` | `{ run }` at the first step                    |
| `POST /codemode` | `{ code }`                                     | runs sandbox code, e.g. `workflow.readState()` |
| `GET /pending`   | none                                           | `{ pending }` actions awaiting approval        |
| `POST /approve`  | `{ executionId }`                              | replays the run, applies the approved call     |
| `POST /reject`   | `{ seq, executionId }`                         | ends the paused execution                      |

## Connector

`workflow` exposes `readState`, `applyCall`, and `pauseStep`. Both writes carry
`requiresApproval`, so the journal changes only after a person approves, and each
returns the reducer's union unchanged. `applyCall` lifts a pause first.

## Running locally

`bun install` at the repo root, then:

```sh
MINIFLARE_WORKERD_PATH=/tmp/workerd-nix \
  nix shell nixpkgs#nodejs_24 --command ./node_modules/.bin/wrangler dev \
  --local --config apps/agent/wrangler.jsonc
```

The npm `workerd` binary cannot start on NixOS, so the flake packages a patched copy
and `MINIFLARE_WORKERD_PATH` points `miniflare` at it; wrangler itself needs the Node
that `nix shell` provides. Code Mode and Pi (`@earendil-works/*`) are experimental and
may break between releases.
