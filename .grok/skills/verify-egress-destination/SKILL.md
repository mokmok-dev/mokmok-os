---
name: verify-egress-destination
description: Model-check the mokmok-egress destination classifier with Kani before or after changing it. Use when a change touches AddressClass, classify, Rules, public_only, Validated::new, or any address range, and when asked to verify that egress admits only public destinations.
when-to-use: A change touches mokmok-egress/src/destination.rs or mokmok-egress/src/connect.rs; a new address class or range is added; a review asks whether public_only can reach a private, shared, loopback, link-local, reserved, or benchmarking address; or the egress acceptance row is being rechecked.
---

# Verify the egress destination classifier

## The promise, and the harm

`Rules::public_only()` is the egress default: a sandbox is handed only the socket addresses the
proxy accepted. That promise holds only if `classify` calls an address `Public` exactly when the
address is globally routable. A false `Public` hands a workload a route into a shared, reserved,
benchmarking, or internal network, which is the class of destination the proxy exists to refuse.

The check in `verification/egress-destination` asserts, for every IPv4 and every IPv6 address:

> If `classify(address) == AddressClass::Public`, then
> `std::net::{Ipv4Addr,Ipv6Addr}::is_global(address)` is true.

An IPv4-mapped IPv6 address is judged by the IPv4 address it carries, the rule
`destination.rs` documents. The specification of "globally routable" is the standard library's
`is_global`, not a hand-written range table.

## Run it

One command, from the repository root:

```sh
bash scripts/verify-egress.sh
```

The script downloads Kani `0.68.0` once into `${XDG_CACHE_HOME:-$HOME/.cache}/mokmok/kani`, checks
the pinned SHA-256, and reuses it. Kani `0.68.0` is built against `nightly-2026-08-21`; install it
with `rustup toolchain install nightly-2026-08-21` if the script says it is missing. The run is a few
seconds after the first build.

On NixOS the release binaries need an interpreter patch, and the host toolchain's `ld.lld` shim may
point at a garbage-collected wrapper; the script applies both when it sees them, keeping a
`ld.lld.mokmok-backup` beside the shim. If the patch step says `patchelf` is missing, run the script
through `nix shell nixpkgs#patchelf --command bash scripts/verify-egress.sh`.

Arguments pass through to Kani. To run one harness:

```sh
bash scripts/verify-egress.sh --harness a_public_ipv4_address_is_globally_routable
```

To get a concrete address for a failing harness, add `--output-format regular` and
`-Z concrete-playback --concrete-playback print`, then read the generated test's `concrete_vals`.
The bytes are the address's memory image: a `u32` or `u128` in little-endian order.

## Current result at `c623b1d`

**The check fails. This is a finding, not a harness error.**

| Harness | Result |
| --- | --- |
| `a_public_ipv4_address_is_globally_routable` | **FAIL** |
| `a_public_ipv6_address_is_globally_routable` | **FAIL** |
| `an_ipv4_mapped_address_is_classified_as_its_ipv4_address` | pass |
| `a_loopback_address_is_loopback` | pass |
| `a_private_address_is_private` | pass |
| `a_link_local_address_is_link_local` | pass |
| `a_forbidden_class_is_never_allowed` | pass |
| `an_unpermitted_class_is_never_allowed` | pass |
| `a_permitted_and_unforbidden_class_is_allowed` | pass |

The formal witnesses are `255.0.31.255` (in `240.0.0.0/4`) and
`2001:1ff:ffff:ffff:ffff:7f7f:ffff:ffff` (in `2001::/23`). Both are `Public` and both are
`is_global == false`.

A probe of representative addresses from the special-purpose registries shows the same gap on this
tree. The addresses below are concrete `classify` inputs; the ranges are the blocks they belong to.

| Address | Block | `classify` | `is_global` |
| --- | --- | --- | --- |
| `0.0.0.1` | `0.0.0.0/8` | Public | false |
| `100.64.0.1` | shared address space | Public | false |
| `192.0.0.1` | IETF protocol assignments | Public | false |
| `198.18.0.1` | benchmarking | Public | false |
| `240.0.0.1` | reserved | Public | false |
| `255.0.31.255` | reserved | Public | false |
| `100::1` | discard-only | Public | false |
| `2001::1` | Teredo | Public | false |
| `2001:2::1` | benchmarking | Public | false |
| `2001:1ff::1` | IETF protocol assignments | Public | false |
| `2002:7f00:1::` | 6to4 | Public | false |
| `3fff::1` | documentation | Public | false |
| `5f00::1` | segment routing | Public | false |
| `64:ff9b:1::7f00:1` | local-use NAT64 | Public | false |

The harm is latent today because the kernel does not compose the egress proxy; the acceptance row
says the proxy has no listener. It lands when the proxy is wired, and the fix belongs in
`classify_v4`/`classify_v6`, not in this harness. Do not weaken the property, the specification, or
the address set to make the suite green.

Two ranges are **not** covered by the property because this toolchain's `is_global` says true for
them, even though they can embed an IPv4 destination: NAT64 `64:ff9b::/96` and IPv4-compatible
`::a.b.c.d`. `classify` also calls them `Public`. That is a separate open question about whether the
classifier should unwrap them; do not read the property's silence as approval.

## Covered code

Checked at commit `c623b1d` (the tree the harnesses last described). The check covers:

| File | Functions |
| --- | --- |
| `mokmok-egress/src/destination.rs` | `classify`, `classify_v4`, `classify_v6`, `is_documentation_v6`, `Rules::permit`, `Rules::forbid`, `Rules::allows` |
| `mokmok-egress/src/connect.rs` | the `Rules::allows(classify(address.ip()))` predicate `Validated::new` applies; both functions are checked, the call site is not executed |

`Validated::new`'s list loop itself is not verified: its audit record reads the clock, and Kani
reports `clock_gettime` as an unsupported foreign call. The predicate it uses is the verified part,
and the loop is one `if` over the list.

## Telling a real bug from a modeling error

- A failure of `a_public_ipv4_address_is_globally_routable` or
  `a_public_ipv6_address_is_globally_routable` is a real gap when the witness is a concrete address
  whose `is_global` is false. Reproduce it with a one-line `classify` call before reporting it.
- A failure of `a_loopback_address_is_loopback`, `a_private_address_is_private`, or
  `a_link_local_address_is_link_local` means a named range stopped being classified as itself.
- A failure of the three `Rules` harnesses means permit/forbid precedence broke; the code's own
  tests in `mokmok-egress/tests/enforcement.rs` cover the same shape.
- An `unsupported construct` or `clock_gettime` failure is a harness path reaching outside the
  model. This harness is built to stop at `classify` and `Rules`; if it starts reaching the clock,
  the harness imported a path it should not have, and that is a harness fix.
- If a witness is `Public` but `is_global` is true, the property is not violated. Recheck the
  toolchain: the spec moves with the pinned nightly, and a different toolchain is a different spec.

## Keeping it in step

1. Check whether the change touches the covered code:
   `git diff c623b1d..HEAD -- mokmok-egress/src/destination.rs mokmok-egress/src/connect.rs`.
2. Run `bash scripts/verify-egress.sh`.
3. If the only failures are the two current findings, the change still describes this tree. If a
   new harness fails, follow the section above.
4. When `classify` is fixed, the two failing harnesses should pass. Delete the finding section,
   keep the harnesses, and move the sync commit.
5. When a class is added to `AddressClass`, add it to `any_class` in
   `verification/egress-destination/src/lib.rs` and to the `match`, or the `Rules` harnesses will
   silently stop covering it.
6. Move the sync commit (`c623b1d` above) only after you have confirmed the harnesses describe the
   code at the new commit. `git rev-parse HEAD` is the value to record.

For a change that did not recheck this verification, `/maintain-formal-verification-skill` brings the
record back in line.

## Bounds and what is left out

- IPv4 and IPv6 are exhaustive: all `2^32` and all `2^128` values. There is no input bound on the
  address. The only bound is the address-list length in `Validated::new`, which is not part of the
  proven predicate.
- `classify` is pure and reads no clock, file, or socket, so there is no environment assumption
  beyond the pinned toolchain.
- Not covered: DNS resolution, the `PolicyClient` decision, the actual `TcpStream::connect`, the
  audit record, platform differences in `is_global`, and the NAT64/IPv4-compatible embeddings
  named above.
