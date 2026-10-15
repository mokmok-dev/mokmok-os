# Executors

* [Run one sanctioned command against the repository](executors/run-in-repo.md) - how a runner produces a receipt
* [Produce a test log from the nix gate](executors/run-nix-check.md) - how to get the nextest summary out of a check whose store path is already built

# Attesters

* [okf-conformance.sh](attesters/okf-conformance.sh) - checks a bundle against OKF v0.2 sections 8, 9, and 11, and resolves every link
* [dependency-rules.sh](attesters/dependency-rules.sh) - recomputes the six dependency rules from the manifests and sources
* [nextest-count.sh](attesters/nextest-count.sh) - re-reads a captured test log and holds a receipt to it
* [line-references.sh](attesters/line-references.sh) - checks every symbol and line citation in a document against the tree it cites
