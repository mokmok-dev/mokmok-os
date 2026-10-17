# Contracts and durability

* [mokmok-protocol](protocol.md) - typed identifiers and the serde message contracts
* [mokmok-event-store](event-store.md) - one SQLite file holding the ordered commit log, streams, projections, and the outbox
* [mokmok-event-bus](event-bus.md) - the WebSocket server on a Unix socket and the delivery and checkpoint contract

# Domain and orchestration

* [mokmok-state-machine](state-machine.md) - pure folds of session facts and turn facts
* [mokmok-agent-runtime](agent-runtime.md) - the decision over the transcript fold and the intent-before-effect step
* [mokmok-scheduler](scheduler.md) - work claims and fenced leases over persisted timing

# Authority and execution

* [mokmok-capability](capability.md) - what a principal may do to a resource, from a policy and a grant tree that only narrows
* [mokmok-sandbox](sandbox.md) - one command under a Landlock boundary, gated by issued capabilities
* [mokmok-egress](egress.md) - destination validation and a connection pinned to the validated addresses

# Data and operations

* [mokmok-artifact](artifact.md) - content-addressed blob storage with references, retention, and collection
* [mokmok-observability](observability.md) - traces, metrics, logs, and the durable audit journal
* [mokmok-search](search.md) - a BM25 index over a directory tree, answering the model's find
* [mokmok-patch](patch.md) - a unified diff read into recomputed counts, applied to bytes with no file opened
* [mokmok-skill](skill.md) - a SKILL.md read into a validated skill, with its name checked against its directory
* [mokmok-kernel](kernel.md) - the composition root, recovery, the vertical slice, and the daemon

# Applications

* [mokmok-tui](tui.md) - a terminal conversation with the agent
