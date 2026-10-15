---
type: Executor
title: Produce a test log from the nix gate
description: How to get the cargo-nextest output out of a flake check whose store path is already built.
resource: ../../../..
tags: [mokmok-os, executor, nix]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
sources:
  - id: flake
    resource: ../../../../flake.nix
    title: the flake that declares the checks
    last_modified: 2026-10-12T16:23:50Z
  - id: repo
    resource: ../../../..
    title: the mokmok-os repository
    last_modified: 2026-10-14T03:34:14Z
---

# The problem

`nix flake check --print-build-logs` prints the nextest summary only when it builds the check. On a
warm store it reports `checks.x86_64-linux.cargoTest (previously built)` and the test count never
reaches the terminal, so a receipt built from that output has no observed count to attest.[^flake]

# Steps

1. Change to the repository root.
2. Run the check with a forced rebuild and capture everything.

```
nix build .#checks.x86_64-linux.cargoTest --rebuild --print-build-logs > /tmp/ci-gate.log 2>&1
```

3. Record the exit status. `--rebuild` compares the rebuilt output with the stored one and exits
   nonzero when they differ, which they do here because the check installs a compressed copy of
   `target/`. The status is therefore a fact about the comparison, and the nextest summary in the
   log is the fact about the tests.
4. Write the receipt with `command`, `exit_code`, `tests_run`, `failed`, and `log`.

# What the run means

The rebuild executes the same derivation the flake check executes, from the same store inputs, so
its nextest summary is the gate's own result. It is a rerun of one check, which is what a reader
needs and is cheaper than a cold `nix flake check`.

[^flake]: `flake.nix` declares `cargoTest = craneLib.cargoNextest`, and crane runs nextest inside the
    nix build sandbox, so the summary is a build log line.
