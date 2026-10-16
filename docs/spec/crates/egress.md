---
type: Crate
title: mokmok-egress
description: Validates a network destination against address-class rules and a policy port, then connects only to the addresses that passed.
resource: ../../../mokmok-egress
tags: [mokmok-os, crate, security, network]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.9
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-egress
    title: mokmok-egress sources
    last_modified: 2026-10-12T06:37:04Z
---

# Responsibility

Decides whether a destination is reachable and pins the decision to the addresses it validated.
Name resolution and authorization both enter through ports the caller supplies, so the crate holds
no resolver and no policy of its own.[^crate]

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `Validated` | `src/connect.rs:18` | The validated destination, its pinned addresses, and `connect` |
| `Destination`, `Host`, `Port`, `Scheme` | `src/destination.rs:45,16,27,7` | The parsed target |
| `AddressClass`, `Rules`, `classify` | `src/destination.rs:60,146,86` | Address classes and the permit and forbid sets |
| `Resolver` | `src/dns.rs:10` | The resolution port |
| `PolicyClient`, `CallContext`, `Refusal` | `src/policy_client.rs:34,9,18` | The authorization port and its answer |
| `parse_connect`, `parse_authority`, `ParseError` | `src/proxy.rs:51,78,9` | The `CONNECT` request line and the authority form |
| `AuditRecord`, `AuditSink`, `DecisionName` | `src/audit.rs:27,45,13` | The record handed to the caller's sink |
| `RateLimit` | `src/rate_limit.rs:17` | A per-principal token bucket |

# Owned state

Nothing durable and nothing on disk. `Validated` holds a destination, a vector of socket addresses,
and the call context (`src/connect.rs:17-21`). `RateLimit` holds a capacity, a refill period, and a
map of buckets by principal (`src/rate_limit.rs:17-21`). An `AuditRecord` goes to the caller's
`AuditSink` and the crate stores nothing (`src/audit.rs:43`).

# Invariants

- The connected address is one that was validated. `connect` iterates the pinned addresses
  (`src/connect.rs:113`) and there is no resolver on the type, so a second resolution cannot redirect
  the connection. The audit record carries the peer address the operating system reported
  (`src/connect.rs:118`).
- Address classes are default-deny. An address is admitted when a rule permits it and no rule
  forbids it (`src/destination.rs:189`), and the default rule set is `public_only`
  (`src/destination.rs:157`).
- A port is never zero. `Port::new` answers `None` for zero (`src/destination.rs:31`).
- An IPv4-mapped IPv6 address is classified as its IPv4 address, so the wrapper cannot hide a
  loopback or private class (`src/destination.rs:87`).
- The `CONNECT` parser refuses empty or whitespace fields, an absolute URI, an unbracketed IPv6
  literal, and a zero or out-of-range port (`src/proxy.rs:55,79-135`).
- The crate reads the audit instant itself (`src/audit.rs:55`).
- A rate limit refills in whole periods and never over-fills (`src/rate_limit.rs:52`).

# Dependencies

`mokmok-protocol`, `serde`, `thiserror`, and `tokio` with `io-util`, `net`, `rt`, `sync`, and
`time`. Development adds `tokio` and `uuid`. The crate declares no rusqlite, no axum, and no
`mokmok-capability` dependency.

# Reconciliation

The design names the modules `proxy`, `connect`, `dns`, `destination`, `policy_client`,
`connection`, `rate_limit`, and `audit`.[^architecture] There is no `connection` module; the
connection lives in `src/connect.rs`.

The design gives this crate HTTP and `CONNECT` proxying. There is no listener, no accept loop, and
no forwarding. The crate parses a `CONNECT` request line (`src/proxy.rs:51`) and opens one outbound
connection (`src/connect.rs:107`).

The design has rate limiting enforced. `RateLimit` is re-exported and exercised by its own tests,
and no other code in the workspace calls it.

The design has DNS and destination checks in this crate. Resolution is a port
(`src/dns.rs:6-21`) with no implementation here; the kernel supplies `SystemResolver` at
`mokmok-kernel/src/provider.rs:295`.

The design requires that enforcement semantics stay aligned with the capability authority. The
crate holds no capability dependency, and authorization arrives through the `PolicyClient` port
(`src/policy_client.rs:36`), which the caller implements.

Behaviour the design does not name: the scheme inferred for port 443 (`src/proxy.rs:117`),
IPv4-mapped classification, the `public_only` default, `DecisionName::Unreachable`, and the audit
clock read inside the crate.

# Tests

25 `#[test]` cases and 4 `#[tokio::test]` cases, in three files and no `#[cfg(test)]` module.

`tests/parsing.rs` holds 18 cases for the `CONNECT` grammar, bracketed IPv6 and IPv4-literal
authorities, and the refusals for a missing, non-numeric, zero, or out-of-range port, an absolute
URI, embedded whitespace, an empty host, a malformed IPv6 literal, and a line that is not a request.
`tests/enforcement.rs` holds 4 cases and 4 asynchronous cases: rules admitting only what they
permit, validation keeping only admitted addresses, a forbidden address refused before any
connection, a policy refusal refusing the connection, a second resolution unable to redirect, a
permitted loopback reaching its listener, and the sink receiving the connected address.
`tests/rate_limit.rs` holds 3 cases for refill timing and per-caller spending.

```
cargo test -p mokmok-egress
```

[^architecture]: Section 5.9 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-egress/src/lib.rs:72-78`.
