//! The sessions the log holds, as a chooser reads them.
//!
//! A session's stream is its id, so the commit log is already the list of sessions and no registry
//! beside it has to agree: [`sessions`] reads the log and folds each session's own facts. What a
//! chooser needs beside the identity is the phase it reached, the goal it was given, and how far it
//! got, and all three are in the history the session wrote.

use std::path::PathBuf;

use mokmok_agent_runtime::Record;
use mokmok_protocol::SessionID;
use mokmok_state_machine::Phase;

use crate::config::Config;
use crate::db::Db;
use crate::error::Error;

/// One session the log holds, and as much of it as a chooser shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Summary {
    /// The session's identity, which is also the stream its history lives in.
    pub session: SessionID,
    /// The phase the session's own facts fold to.
    pub phase: Phase,
    /// The goal the session's task named, when it committed one.
    pub goal: Option<String>,
    /// The checkout that task named, when it committed one.
    pub checkout: Option<PathBuf>,
    /// How many turns the session opened.
    pub turns: u64,
}

/// Every session the log holds, the one written most recently first.
///
/// A stream that holds no session fact is a session this build cannot name, and it is listed with
/// the phase a fold with no facts reaches rather than skipped: a chooser that hid it would be
/// hiding a history that is really there.
///
/// # Errors
///
/// Returns the errors of opening the log, of reading one session's history, and of the fold that
/// reads its phase.
pub fn sessions(config: &Config) -> Result<Vec<Summary>, Error> {
    let db = Db::open(&config.events)?;

    db.streams()?
        .into_iter()
        .map(|stream| {
            let session = SessionID::from_uuid(stream.as_uuid());
            let log = db.log(session)?;
            let task = log.ordered.iter().find_map(|record| match record {
                Record::Task(record) => Some(record.task.clone()),
                _ => None,
            });

            Ok(Summary {
                session,
                phase: log.state(session)?.phase(),
                goal: task.as_ref().map(|task| task.goal.clone()),
                checkout: task.map(|task| task.setting.checkout),
                turns: u64::try_from(log.turns_in_order().len()).unwrap_or(u64::MAX),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::path::PathBuf;
    use std::time::Duration;

    use clap::Parser as _;
    use mokmok_agent_runtime::{Budget, Record, Setting, Task, TaskRecord, turn_id};
    use mokmok_event_store::{CommandId, Expected, Outcome};
    use mokmok_protocol::SessionID;
    use mokmok_state_machine::{Fact, Phase, TurnFact, TurnNumber};
    use uuid::Uuid;

    use super::sessions;
    use crate::config::{Args, Config, ProcessEnvironment};
    use crate::db::{Db, Entry};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "mokmok-kernel-roster-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);

            Self(path)
        }

        fn data(&self) -> PathBuf {
            self.0.join("data")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The first shell on `PATH`, which stands in for the confiner: nothing here confines anything.
    fn shell() -> PathBuf {
        let path = std::env::var_os("PATH").expect("a PATH");

        for directory in std::env::split_paths(&path) {
            for name in ["sh", "bash"] {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }

        panic!("no shell on PATH");
    }

    fn config(data: &Path) -> Config {
        let args = Args::try_parse_from([
            "mokmokd",
            "--data-dir",
            &data.display().to_string(),
            "--confiner",
            &shell().display().to_string(),
            "status",
            "--session",
            "018f2f1e-0000-7000-8000-000000000000",
        ])
        .expect("the command line parses");

        Config::resolve(&args, None, &ProcessEnvironment::default())
            .expect("the configuration resolves")
    }

    fn session(seed: u128) -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(seed))
    }

    fn task() -> Task {
        Task {
            goal: "hold a conversation".to_owned(),
            setting: Setting {
                checkout: PathBuf::from("/checkout"),
                readable: Vec::new(),
                writable: Vec::new(),
                program: PathBuf::from("/bin/sh"),
                deadline: Duration::from_secs(1),
                search: Vec::new(),
            },
            budget: Budget::default(),
        }
    }

    fn write(
        db: &mut Db,
        session: SessionID,
        entries: &[Entry],
    ) {
        let expected = match db.version(Db::stream(session)).expect("the version reads") {
            version if version.get() == 0 => Expected::New,
            version => Expected::At(version),
        };
        let outcome = db
            .append(
                CommandId::new(),
                Db::stream(session),
                expected,
                entries,
                Vec::new(),
            )
            .expect("the commit lands");

        assert!(matches!(outcome, Outcome::Committed(_)));
    }

    fn opened() -> Entry {
        Entry::Session(Fact::SessionOpened)
    }

    #[test]
    fn an_empty_log_lists_no_session() {
        let scratch = Scratch::new("empty");
        let config = config(&scratch.data());

        assert_eq!(sessions(&config).expect("the log is read"), Vec::new());
    }

    #[test]
    fn the_session_written_last_comes_first() {
        let scratch = Scratch::new("order");
        let config = config(&scratch.data());
        let older = session(1);
        let newer = session(2);
        let mut db = Db::open(&config.events).expect("the log opens");

        write(&mut db, older, &[opened()]);
        write(&mut db, newer, &[opened()]);
        // The older one moves afterwards, so the order cannot be the order of first writes.
        write(&mut db, older, &[Entry::Session(Fact::CancelRequested)]);

        let listed = sessions(&config).expect("the log is read");

        assert_eq!(
            listed,
            vec![
                super::Summary {
                    session: older,
                    phase: Phase::Draining,
                    goal: None,
                    checkout: None,
                    turns: 0,
                },
                super::Summary {
                    session: newer,
                    phase: Phase::Accepting,
                    goal: None,
                    checkout: None,
                    turns: 0,
                },
            ],
            "the session written most recently is listed first",
        );
    }

    #[test]
    fn a_session_that_recorded_a_task_carries_its_goal_and_its_checkout() {
        let scratch = Scratch::new("task");
        let config = config(&scratch.data());
        let mut db = Db::open(&config.events).expect("the log opens");
        let named = session(1);
        let bare = session(2);

        write(
            &mut db,
            named,
            &[
                opened(),
                Entry::Agent(Record::Task(TaskRecord { task: task() })),
            ],
        );
        write(&mut db, bare, &[opened()]);

        let listed = sessions(&config).expect("the log is read");
        let held = |id| {
            listed
                .iter()
                .find(|summary| summary.session == id)
                .expect("the session is listed")
        };

        assert_eq!(held(named).goal.as_deref(), Some("hold a conversation"));
        assert_eq!(
            held(named).checkout.as_deref(),
            Some(Path::new("/checkout")),
        );
        assert_eq!(held(bare).goal, None);
        assert_eq!(held(bare).checkout, None);
    }

    #[test]
    fn the_turn_count_is_the_turns_the_session_opened() {
        let scratch = Scratch::new("turns");
        let config = config(&scratch.data());
        let mut db = Db::open(&config.events).expect("the log opens");
        let id = session(1);

        write(
            &mut db,
            id,
            &[
                opened(),
                Entry::Turn(
                    turn_id(id, 1),
                    TurnFact::TurnOpened {
                        number: TurnNumber::new(1),
                    },
                ),
                Entry::Turn(
                    turn_id(id, 2),
                    TurnFact::TurnOpened {
                        number: TurnNumber::new(2),
                    },
                ),
            ],
        );

        assert_eq!(
            sessions(&config).expect("the log is read")[0].turns,
            2,
            "both turns the session opened are counted",
        );
    }

    #[test]
    fn a_stream_with_no_session_fact_is_listed_as_a_session_that_never_opened() {
        let scratch = Scratch::new("bare");
        let config = config(&scratch.data());
        let mut db = Db::open(&config.events).expect("the log opens");
        let id = session(1);

        write(
            &mut db,
            id,
            &[Entry::Turn(
                turn_id(id, 1),
                TurnFact::TurnOpened {
                    number: TurnNumber::new(1),
                },
            )],
        );

        assert_eq!(
            sessions(&config).expect("the log is read"),
            vec![super::Summary {
                session: id,
                phase: Phase::Unopened,
                goal: None,
                checkout: None,
                turns: 1,
            }],
        );
    }
}
