# Bundle update log

## 2026-10-15
* **Creation**: Wrote the bundle from `mokmok-os-architecture.md` and the source tree at commit `bbe2ea2`, against OKF v0.2.
* **Creation**: Split the twelve target crate boundaries into one concept per workspace member, and added `mokmok-tui`, which the design does not cover.
* **Creation**: Added four attested computations, with their executors and attesters under `references/`: the dependency rules, the nextest test count, bundle conformance, and the line references.
* **Observation**: The nix gate ran 608 test cases, none failing, one skipped. The six dependency rules passed. The bundle reported zero conformance problems, and 570 symbol and line citations resolved.
* **Addition**: Added `mokmok-search`, the fourteenth member, and its concept document. The design names no boundary for it; it is a data and retrieval crate under the kernel.
* **Change**: The agent runtime gained a second tool, `find_files`, behind a granted search root, and the kernel now composes one corpus per root and answers it. The bundle's port table carries the `Finder` row, and the citations in the agent-runtime, kernel, and tui concepts were rewritten where the change moved a line.
