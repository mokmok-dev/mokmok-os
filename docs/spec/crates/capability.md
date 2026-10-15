---
type: Crate
title: mokmok-capability
description: Decides which principal may act on a resource in one context, from a policy, a grant tree that can only narrow, recorded revocations, and approval requirements.
resource: ../../../mokmok-capability
tags: [mokmok-os, crate, security]
status: draft
generated: { by: grok-build/grok-4.7, at: 2026-10-15T02:00:00Z }
stale_after: 2026-11-15T00:00:00Z
sources:
  - id: architecture
    resource: ../../../mokmok-os-architecture.md
    title: mokmok OS architecture and crate responsibilities, section 5.7
    author: codex/gpt-6
  - id: crate
    resource: ../../../mokmok-capability
    title: mokmok-capability sources
    last_modified: 2026-10-12T06:37:04Z
---

# Responsibility

Decides authority. `Authority::decide` answers with an allowed grant, a denial reason, or a
requirement that someone approve the operation, and the crate holds no network path, no process
control, and no clock of its own.[^crate]

# Public surface

| Item | Where | What it is |
| --- | --- | --- |
| `Authority` | `src/authority.rs:26` | `new`, `since`, `principal`, `issued_at`, `root`, `issue`, `decide`, `approve`, `revoke`, `revoke_principal` |
| `Policy`, `Rule` | `src/policy.rs:193,56` | A rule set and its builder for principal, action, resource, context, and expiry |
| `Principal`, `PrincipalKind` | `src/principals.rs:22,11` | The actors |
| `Resource`, `Name` | `src/resource.rs:9`, `src/name.rs:14` | The things they act on, and a validated label |
| `Action`, `ActionSet`, `Permission` | `src/permission.rs:10,38,106` | Component-wise coverage of an action set over one resource |
| `Grant`, `GrantId`, `Attenuation` | `src/grants.rs:36,13,124` | The issued authority and the refusal to narrow it |
| `Decision`, `Denial`, `DenialReason`, `Request`, `Context` | `src/decision.rs:45,56,85,36,16` | The three answers and what they were asked |
| `Requirement`, `ApprovalError` | `src/approvals.rs:64,133` | The approval binding and why one was refused |

The modules `approvals`, `authority`, `decision`, `grants`, `name`, `permission`, `policy`,
`principals`, `resource`, and `revocations` are private. The list above is the re-export set
(`src/lib.rs:83-91`).

# Owned state

Nothing durable and nothing outside memory. `Authority::new` (`src/authority.rs:38-50`) builds a
policy, a `since` instant, maps of principals and held grants, a revocation set, and the next grant
number. A revocation keeps the grant and records the instant of the refusal
(`src/revocations.rs:8-25`). The crate is `#![no_std]`, so it holds no file, no socket, and no
random source.

# Invariants

- Default denial. A request matching no rule is `Deny(NoMatchingRule)` (`src/authority.rs:162`), and
  `Policy::new` holds no rules (`src/policy.rs:199`).
- A grant cannot be written down. `Grant` has no public constructor and no `Deserialize`
  (`src/grants.rs:30-37`); only the module-private `minted` function builds one (`src/grants.rs:79`).
- Attenuation cannot widen. `issue` refuses a permission the parent does not cover, as `NotASubset`
  (`src/authority.rs:120`), one that outlives the parent, as `OutlivesParent` (`src/authority.rs:124`),
  and one already expired, as `AlreadyExpired` (`src/authority.rs:133`).
- Grant numbers increase (`src/authority.rs:252`).
- Revocation and expiry reach the whole chain. `refusal` walks from the grant to its root
  (`src/authority.rs:317`), and expiry is inclusive of the named instant (`src/authority.rs:327`).
- Approval binds to the operation and the approver's standing. `approve` refuses a self-approval
  (`src/authority.rs:201`), an approver the resource did not admit (`src/authority.rs:205`), and a
  closed window (`src/authority.rs:209`). A narrower child inherits the binding
  (`src/authority.rs:139`).
- Decoded values re-run their construction checks. `ActionSet::Deserialize` refuses an empty or
  unknown bit set (`src/permission.rs:95`), and `Name::Deserialize` re-validates
  (`src/name.rs:89`).
- An `Allow` always names a grant that reaches a root the authority issued
  (`tests/reachability.rs`).

# Dependencies

`mokmok-protocol` and `serde` with `alloc` and `derive`. Development adds `proptest`, `serde_json`,
and `uuid`.
The crate is `#![no_std]` with `extern crate alloc` (`src/lib.rs:68-70`), and it
declares no tokio, nix, or landlock dependency.

# Reconciliation

The design names the modules `principal`, `grant`, `permission`, `policy`, `approval`,
`revocation`, and `decision`.[^architecture] The crate names them `principals`, `grants`,
`approvals`, `revocations`, `permission`, `policy`, and `decision`, and adds `authority` and `name`.

The design requires that authority changes retain durable rationale. Nothing here is durable.
Grants, their issue instants, and revocations live in the `Authority` value's maps, and no
`Authority` serializes. `Grant` serializes outward only, which its own comment explains.

The design allows a durable approval workflow to move into its own `mokmok-approval` crate. No such
crate exists, and the approval requirement here has no lifecycle and no persistence.

Behaviour the design does not name: `Action::Connect`, `Resource::Host`, the DNS-label length limit
on `Name`, `Attenuation::AlreadyExpired`, the severity ordering behind `Denial`, and the absence of
any dependency beyond serde.

The kernel consumes this crate through a wrapper at `mokmok-kernel/src/authority.rs:15-20`, which
builds the policy from command-line flags.

# Tests

49 `#[test]` cases and 1 proptest case across eight files in `tests/` and two `#[cfg(test)]` modules
in `src/`.

`tests/attenuation.rs` holds 5 cases for a wider permission, an outliving child, equal expiry, an
unknown parent, and a past expiry. `tests/lifetime.rs` holds 5 cases for the boundary instant,
grant revocation cascading, principal revocation cascading, the issue instant, and a later grant
surviving. `tests/policy.rs` holds 13 cases for default denial, the most specific rule, a denial
outranking an approval, tie-break order, and per-principal scope. `tests/reachability.rs` asserts as
a property that every `Allow` names a grant attenuated from a root. `tests/approval.rs` holds 5
cases for the approval binding.

```
cargo test -p mokmok-capability
```

[^architecture]: Section 5.7 of `mokmok-os-architecture.md`.
[^crate]: The re-export list is `mokmok-capability/src/lib.rs:83-91`.
