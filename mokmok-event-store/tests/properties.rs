use mokmok_event_store::{
    Append, CommandId, CommitPosition, Committed, ConsumerId, Error, Expected, Kind, NewEvent,
    Outcome, Payload, Processed, Store, StreamId, Version,
};
use proptest::collection::vec;
use proptest::prelude::*;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::path::Path;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use super::*;

    const STREAMS: usize = 3;
    const CONSUMERS: usize = 2;

    #[derive(Clone, Copy, Debug)]
    enum Op {
        Append {
            stream: usize,
            expected: Expected,
            events: usize,
        },
        Repeat {
            command: usize,
        },
        Consume {
            consumer: usize,
            commit: usize,
            project: bool,
        },
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0..STREAMS, expected(), 1_usize..4).prop_map(|(stream, expected, events)| {
                Op::Append {
                    stream,
                    expected,
                    events,
                }
            }),
            (0..8_usize).prop_map(|command| Op::Repeat { command }),
            (0..CONSUMERS, 0..8_usize, any::<bool>()).prop_map(|(consumer, commit, project)| {
                Op::Consume {
                    consumer,
                    commit,
                    project,
                }
            }),
        ]
    }

    fn expected() -> impl Strategy<Value = Expected> {
        prop_oneof![
            Just(Expected::Any),
            Just(Expected::New),
            (0_u64..8).prop_map(|at| Expected::At(Version::new(at))),
        ]
    }

    #[derive(Debug, Default)]
    struct Model {
        versions: BTreeMap<StreamId, Version>,
        commands: Vec<CommandId>,
        commits: Vec<ModelCommit>,
        checkpoint: BTreeMap<ConsumerId, CommitPosition>,
        inbox: BTreeSet<(ConsumerId, u64)>,
    }

    #[derive(Debug)]
    struct ModelCommit {
        position: u64,
        stream: StreamId,
        first: u64,
        events: usize,
    }

    impl Model {
        fn version(
            &self,
            stream: StreamId,
        ) -> Version {
            self.versions
                .get(&stream)
                .copied()
                .unwrap_or(Version::INITIAL)
        }

        fn record(
            &mut self,
            stream: StreamId,
            command: CommandId,
            commit: &Committed,
        ) {
            let Some(first) = commit.events().first() else {
                panic!("a commit always carries at least one event");
            };

            self.versions.insert(stream, commit.version());
            self.commands.push(command);
            self.commits.push(ModelCommit {
                position: commit.position().get(),
                stream,
                first: first.version().get(),
                events: commit.events().len(),
            });
        }

        fn advance(
            &mut self,
            consumer: ConsumerId,
            position: CommitPosition,
        ) {
            let current = self.checkpoint.get(&consumer).copied();
            let next = current.map_or(position, |held| {
                if held.get() >= position.get() {
                    held
                } else {
                    position
                }
            });

            self.checkpoint.insert(consumer, next);
        }
    }

    fn max(count: usize) -> NonZeroUsize {
        NonZeroUsize::new(count).unwrap()
    }

    fn event(index: usize) -> NewEvent {
        NewEvent::new(
            Kind::new(format!("turn.{index}")).unwrap(),
            Payload::json(&serde_json::json!({"n": index})).unwrap(),
        )
    }

    fn build(
        command: CommandId,
        stream: StreamId,
        expected: Expected,
        events: usize,
    ) -> Append {
        let mut append = Append::new(command, stream, expected, event(0));
        for index in 1..events {
            append = append.event(event(index));
        }

        append
    }

    fn apply(
        store: &mut Store,
        model: &mut Model,
        streams: &[StreamId],
        consumers: &[ConsumerId],
        step: Op,
    ) {
        match step {
            Op::Append {
                stream,
                expected,
                events,
            } => apply_append(
                store,
                model,
                streams[stream % streams.len()],
                expected,
                events,
            ),
            Op::Repeat { command } => apply_repeat(store, model, command),
            Op::Consume {
                consumer,
                commit,
                project,
            } => apply_consume(
                store,
                model,
                consumers[consumer % consumers.len()],
                commit,
                project,
            ),
        }
    }

    fn apply_append(
        store: &mut Store,
        model: &mut Model,
        stream: StreamId,
        expected: Expected,
        events: usize,
    ) {
        let found = model.version(stream);
        let total = store.log(CommitPosition::FIRST, max(64)).unwrap().len();
        let command = CommandId::new();
        let outcome = store
            .append(&build(command, stream, expected, events))
            .unwrap();

        match outcome {
            Outcome::Committed(commit) => {
                assert!(
                    expected.accepts(found),
                    "the store accepted an expectation the model refused",
                );
                model.record(stream, command, &commit);
            },
            Outcome::Conflicted {
                expected: reported,
                found: reported_found,
            } => {
                assert!(
                    !expected.accepts(found),
                    "the store refused an expectation the model accepted",
                );
                assert_eq!(reported, expected);
                assert_eq!(reported_found, found);
                assert_eq!(
                    store.log(CommitPosition::FIRST, max(64)).unwrap().len(),
                    total,
                    "a refused expectation writes nothing at all",
                );
            },
            Outcome::Replayed(_) => panic!("a fresh command id replayed"),
        }
    }

    fn apply_repeat(
        store: &mut Store,
        model: &Model,
        index: usize,
    ) {
        if model.commands.is_empty() {
            return;
        }

        let index = index % model.commands.len();
        let command = model.commands[index];
        let original = &model.commits[index];
        let total = store.log(CommitPosition::FIRST, max(64)).unwrap().len();
        let outcome = store
            .append(&build(command, original.stream, Expected::Any, 1))
            .unwrap();
        let Outcome::Replayed(replayed) = outcome else {
            panic!("a repeated command did not replay: {outcome:?}");
        };

        assert_eq!(
            replayed.position().get(),
            original.position,
            "a replay keeps its position"
        );
        assert_eq!(
            replayed.events().len(),
            original.events,
            "a replay keeps its events"
        );
        assert_eq!(
            store.log(CommitPosition::FIRST, max(64)).unwrap().len(),
            total,
            "a replay adds no commit",
        );
    }

    fn apply_consume(
        store: &mut Store,
        model: &mut Model,
        consumer: ConsumerId,
        commit_index: usize,
        project: bool,
    ) {
        if model.commits.is_empty() {
            return;
        }

        let log = store.log(CommitPosition::FIRST, max(64)).unwrap();
        let commit = &log[commit_index % model.commits.len()];
        let position = commit.position();
        let key = (consumer, position.get());
        let already = model.inbox.contains(&key);
        let calls = Cell::new(0_usize);
        let payload = commit.events()[0].payload();

        let result = store
            .consume(consumer, commit, |tx| -> Result<(), Error> {
                calls.set(calls.get() + 1);
                if project {
                    tx.put_projection(consumer, "prop", payload, position)?;
                    assert_eq!(
                        tx.projection(consumer, "prop")?
                            .map(|seen| seen.as_str().to_owned()),
                        Some(payload.as_str().to_owned()),
                        "a projection reads back inside the transaction",
                    );
                }
                Ok(())
            })
            .unwrap();

        if already {
            assert!(
                matches!(result, Processed::Duplicate),
                "a redelivery is a duplicate"
            );
            assert_eq!(calls.get(), 0, "a duplicate does not run the closure");
        } else {
            assert!(
                matches!(result, Processed::Applied(())),
                "a first delivery applies"
            );
            assert_eq!(calls.get(), 1, "a position delivered again applies once");
            model.inbox.insert(key);
            model.advance(consumer, position);
        }
    }

    fn assert_agrees(
        store: &Store,
        model: &Model,
        streams: &[StreamId],
        consumers: &[ConsumerId],
    ) {
        let log = store.log(CommitPosition::FIRST, max(64)).unwrap();
        assert_eq!(
            log.len(),
            model.commits.len(),
            "the log holds the model's commits"
        );

        let positions = log
            .iter()
            .map(|commit| commit.position().get())
            .collect::<Vec<_>>();
        let total = u64::try_from(log.len()).unwrap();
        assert_eq!(
            positions,
            (1..=total).collect::<Vec<_>>(),
            "positions are a permutation of 1..=total and position order is commit order",
        );

        for (commit, expected) in log.iter().zip(&model.commits) {
            assert_eq!(commit.position().get(), expected.position);
            assert_eq!(commit.events().len(), expected.events);
            assert_eq!(commit.events()[0].stream(), expected.stream);
            assert_eq!(commit.events()[0].version().get(), expected.first);
            assert_eq!(
                commit
                    .events()
                    .iter()
                    .map(|event| event.version().get())
                    .collect::<Vec<_>>(),
                (expected.first..expected.first + u64::try_from(expected.events).unwrap())
                    .collect::<Vec<_>>(),
                "a commit's events are its version range in order",
            );
        }

        for stream in streams {
            let slice = store.history(*stream, Version::INITIAL, max(64)).unwrap();
            let version = model.version(*stream);
            assert_eq!(
                slice.version(),
                version,
                "the stream version matches the model"
            );
            assert_eq!(
                slice
                    .events()
                    .map(|event| event.version().get())
                    .collect::<Vec<_>>(),
                (0..version.get()).collect::<Vec<_>>(),
                "the stored versions are exactly 0..version, contiguous and unique",
            );
            assert_eq!(
                slice.commits().len(),
                model
                    .commits
                    .iter()
                    .filter(|commit| commit.stream == *stream)
                    .count(),
                "a stream read returns that stream's commits",
            );
        }

        for consumer in consumers {
            assert_eq!(
                store.checkpoint(*consumer).unwrap(),
                model.checkpoint.get(consumer).copied(),
                "the checkpoint is the running maximum of the delivered positions",
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn the_store_agrees_with_the_model_after_every_operation(script in vec(op(), 1..12)) {
            for version in 0_u64..8 {
                prop_assert_eq!(
                    Version::new(version).next().get(),
                    version + 1,
                    "next steps by exactly one",
                );
            }

            let mut store = Store::open(Path::new(":memory:")).unwrap();
            let streams = (0..STREAMS).map(|_| StreamId::new()).collect::<Vec<_>>();
            let consumers = (0..CONSUMERS).map(|_| ConsumerId::new()).collect::<Vec<_>>();
            let mut model = Model::default();

            for step in script {
                apply(&mut store, &mut model, &streams, &consumers, step);
                assert_agrees(&store, &model, &streams, &consumers);
            }
        }
    }
}
