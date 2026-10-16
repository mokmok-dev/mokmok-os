---
type: Attested Computation
title: The nix gate and its test count
description: Runs the repository's test check and holds the reported test count to the nextest log that run produced.
runtime: nix
tags: [mokmok-os, verification, nix]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
verified:
  - { by: process:nix-flake-check, at: 2026-10-15T01:56:32Z }
  - { by: process:cargoTest-check, at: 2026-10-16T09:05:00Z }
stale_after: 2026-11-15T00:00:00Z
executor:
  resource: /references/executors/run-nix-check.md
  receipt: [command, exit_code, tests_run, failed, log]
attester:
  resource: /references/attesters/nextest-count.sh
  verdict: [verdict, reason]
sources:
  - id: flake
    resource: ../../../flake.nix
    title: the flake that declares the checks
    last_modified: 2026-10-12T16:23:50Z
  - id: repo
    resource: ../../..
    title: the mokmok-os repository at commit bbe2ea2
    last_modified: 2026-10-14T03:34:14Z
---

# Computation

```
nix build .#checks.x86_64-linux.cargoTest --rebuild --print-build-logs
```

The check is the flake's `cargoTest`, which is crane's `cargoNextest` over the whole
workspace.[^flake] The command runs that derivation again so its output reaches a log, for the
reason [the executor](/references/executors/run-nix-check.md) records.

# The run on 2026-10-16, at the head of `feat/credential-env-key`

The receipt below is what the run returned, and
[nextest-count.sh](/references/attesters/nextest-count.sh) returned `verdict=pass` against it.

| Field | Value |
| --- | --- |
| `command` | `nix build .#checks.x86_64-linux.cargoTest --no-link --print-build-logs` |
| `exit_code` | 0 |
| `tests_run` | 881 |
| `failed` | 0 |
| nextest summary | `881 tests run: 881 passed, 1 skipped` |

Every check attribute reported pass: `cargoClippy`, `cargoDocTest`, `pre-commit`, `cargoTest`,
`treefmt`, `nixosModule`, `dependencyRules`, and `mokmokdUnit`.

# What this proves

881 test cases compile and pass in the nix build sandbox at the head of `feat/credential-env-key`,
two more than the head of `feat/tui-sessions` below: the credential case against a stub endpoint and
the credential clause in the confined environment case. The sandbox has a real SQLite, a real Unix
socket, and a real Landlock kernel, so the cases that need those are running against them.

# The run before it, on 2026-10-16, at the head of `feat/tui-sessions`

| Field | Value |
| --- | --- |
| `command` | `nix flake check --print-build-logs` |
| `exit_code` | 0 |
| `tests_run` | 858 |
| `failed` | 0 |
| nextest summary | `858 tests run: 858 passed, 1 skipped` |

Every check attribute reported pass: `cargoClippy`, `cargoDocTest`, `pre-commit`, `cargoTest`,
`treefmt`, `nixosModule`, `dependencyRules`, and `mokmokdUnit`.

# The run before it, on 2026-10-15

| Field | Value |
| --- | --- |
| `command` | `nix build .#checks.x86_64-linux.cargoTest --rebuild --print-build-logs` |
| `exit_code` | 1 |
| `tests_run` | 608 |
| `failed` | 0 |
| nextest summary | `608 tests run: 608 passed, 1 skipped` |

The exit status of 1 comes from `--rebuild` comparing the rebuilt output with the stored one. The
check installs a compressed copy of `target/`, so the two outputs differ and nix reports that. The
attester keys on the nextest summary and on the absence of any failing line, and it reports the
status alongside the verdict. That run measured the repository at commit `bbe2ea2`.

# What this does not prove

It says nothing about isolation on a platform other than `x86_64-linux`, nothing about exactly-once
effects at an external service, and nothing about a design claim no test exercises. It is also not
the mutation gate, which is a separate script the operator runs.

[^flake]: `flake.nix` declares `cargoTest = craneLib.cargoNextest` with `--no-tests=pass`.
