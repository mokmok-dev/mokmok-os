---
okf_version: "0.2"
---

# The specification

* [mokmok OS architecture](architecture.md) - the four commitments, the eight principles, the planes, and the charter
* [The workspace as it stands](workspace.md) - thirteen members, their dependents, and how they map onto the design
* [Port ownership and transaction boundaries](boundaries.md) - where each port is declared, who implements it, and what commits as one unit of work
* [The shared contracts](contracts.md) - message kinds, identifiers, the envelope, versioning, and the contracts that live elsewhere
* [The decision register](decisions.md) - the design's open decisions, the choices the tree made, and the ones the register should carry

# The crates

* [Crates](crates/) - one concept per workspace member, with its contracts, invariants, and what the design asked that the code does not do

# Verification

* [Verification](verification/) - the evidence layers, the acceptance table, and four attested computations

# References

* [References](references/) - the executors and the attesters the computations name
