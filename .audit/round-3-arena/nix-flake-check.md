# The gate, on the branch tip after P0

Extracted from a full `nix flake check --print-build-logs` run (211 KB of build noise) rather than
being the whole log, plus the two commands whose output the check writes to its own store path.

## nix flake check, exit 0

```
✅ nixosModules.default
✅ devShells.x86_64-linux.default (build skipped)
✅ formatter.x86_64-linux (build skipped)
✅ packages.x86_64-linux.default (build skipped)
✅ checks.x86_64-linux.pre-commit
✅ checks.x86_64-linux.treefmt
✅ checks.x86_64-linux.cargoTest
✅ checks.x86_64-linux.cargoClippy
✅ checks.x86_64-linux.nixosModule
✅ checks.x86_64-linux.mokmokdUnit
✅ checks.x86_64-linux.dependencyRules
✅ checks.x86_64-linux.cargoDocTest
```

## the cargoTest summary, from the same log

```
Summary [   5.682s] 757 tests run: 757 passed, 1 skipped
```

## nix build .#checks.x86_64-linux.dependencyRules --no-link --print-out-paths | xargs cat

```
rule=protocol-has-no-domain-dependency status=pass
rule=every-member-declares-its-role status=pass
rule=pure-crates-are-no_std status=pass
rule=pure-crates-depend-only-on-pure-crates status=pass
rule=the-role-census-is-not-empty status=pass detail=pure=4 adapter=9 shell=2
rule=event-store-has-no-transport-dependency status=pass
rule=scheduler-has-no-model-dependency status=pass
rule=capability-performs-no-enforcement-io status=pass
rule=kernel-dependents-are-applications status=pass
```

## cargo test --workspace --all-features, locally

770 passed, 0 failed, 1 ignored. The 757 above is cargo-nextest's count inside the nix sandbox,
which counts cases differently.
