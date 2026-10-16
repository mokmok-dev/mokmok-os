# Bundle update log

## 2026-10-16
* **Change**: The provider group moved into a configuration file. `mokmokd` and `mokmok-tui` read `$XDG_CONFIG_HOME/mokmok/config.toml`, falling back to `$HOME/.config/mokmok/config.toml`, and `--config` names a file instead; a flag beats the file and the file beats the built-in default. The file carries the six provider keys and no authority grant, and a path key in it resolves beside the file. The kernel concept gained the `ProviderLayer` row, four invariants for the file, `toml` in its dependency list, and a recounted test paragraph; the tui concept records that the binary declares no flag of its own; the decision register carries the scope choice, which the design does not name.
* **Change**: The citations into the kernel's `config.rs`, `agent.rs`, `provider.rs`, and two binaries were rewritten where this change moved a symbol, and the numbers that had drifted since `bbe2ea2` were brought up to the tree in the same pass.
* **Observation**: The nix gate ran 809 test cases, none failing, one skipped, and it is green at this commit. The nine dependency rules passed. The bundle reported zero conformance problems over 32 files, and 628 symbol and line citations resolved.

## 2026-10-15
* **Creation**: Wrote the bundle from `mokmok-os-architecture.md` and the source tree at commit `bbe2ea2`, against OKF v0.2.
* **Creation**: Split the twelve target crate boundaries into one concept per workspace member, and added `mokmok-tui`, which the design does not cover.
* **Creation**: Added four attested computations, with their executors and attesters under `references/`: the dependency rules, the nextest test count, bundle conformance, and the line references.
* **Observation**: The nix gate ran 608 test cases, none failing, one skipped. The six dependency rules passed. The bundle reported zero conformance problems, and 570 symbol and line citations resolved.
* **Addition**: Added `mokmok-search`, the fourteenth member, and its concept document. The design names no boundary for it; it is a data and retrieval crate under the kernel.
* **Change**: The agent runtime gained a second tool, `find_files`, behind a granted search root, and the kernel now composes one corpus per root and answers it. The bundle's port table carries the `Finder` row, and the citations in the agent-runtime, kernel, and tui concepts were rewritten where the change moved a line.
