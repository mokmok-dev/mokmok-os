//! The `Engine` over the kernel's own driver, and the `Roster` over the kernel's log.

use mokmok_agent_runtime::{Progress, Transcript};
use mokmok_kernel::agent::Driver;
use mokmok_kernel::{Config, Run};
use mokmok_protocol::{Moment, SessionID};

use crate::Error;
use crate::port::{Engine, Listing, Roster};

/// The engine one session runs on: the kernel's own driver, one bounded step at a time.
///
/// Every value is the one `mokmokd agent run` builds for the same configuration, so a conversation
/// and a run differ in how often they step and in nothing else.
pub struct Session {
    driver: Driver,
}

impl Session {
    /// Opens the session's world: its log, its tool, and the provider the configuration named.
    ///
    /// # Errors
    ///
    /// Returns the errors of the kernel's driver: a refused grant, program, or credential, and the
    /// log, artifact store, sandbox, or provider that cannot be opened.
    pub fn open(
        config: &Config,
        run: &Run,
        at: Moment,
    ) -> Result<Self, Error> {
        let driver = Driver::open(config, run, at)?;

        Ok(Self { driver })
    }

    /// The session this engine works in.
    #[must_use]
    pub const fn session(&self) -> SessionID {
        self.driver.session()
    }
}

impl Engine for Session {
    fn session(&self) -> SessionID {
        Self::session(self)
    }

    fn fold(&self) -> Result<Transcript, Error> {
        self.driver.transcript().map_err(Error::from)
    }

    fn speak(
        &mut self,
        line: &str,
    ) -> Result<(), Error> {
        self.driver.speak(line)?;

        Ok(())
    }

    async fn step(&mut self) -> Result<Progress, Error> {
        self.driver.step().await.map_err(Error::from)
    }

    fn reopen(&mut self) -> Result<(), Error> {
        self.driver.reopen().map_err(Error::from)
    }
}

/// The sessions the log holds, as the terminal opens them.
///
/// The command line names one session and the budgets every session runs under, and the log itself
/// answers for the rest: a session it already holds is resumed from the task that session recorded,
/// so a human can switch to a conversation this process was never told about.
#[derive(Clone, Debug)]
pub struct Sessions {
    config: Config,
    run: Run,
    at: Moment,
}

impl Sessions {
    #[must_use]
    pub const fn new(
        config: Config,
        run: Run,
        at: Moment,
    ) -> Self {
        Self { config, run, at }
    }

    /// The session the command line named: the one the conversation starts in.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Untasked`] when the log holds a taskless history for that session, and the
    /// errors of opening the session's world.
    pub fn start(&self) -> Result<Session, Error> {
        let session = SessionID::from_uuid(mokmok_kernel::uuid_of(&self.run.session, "session")?);
        let run = self.run_for(session)?;

        Session::open(&self.config, &run, self.at)
    }

    /// The run one session is opened with: the command line's own, refused when the log holds a
    /// history for the session and no task in it.
    ///
    /// The goal and the checkout stay the command line's, because the fold prefers the task the log
    /// recorded: a session that recorded one runs under it whatever this carries, and one the log
    /// holds nothing for is opened under the command line's goal.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Untasked`] when the log holds a history for the session and no task in it.
    /// The runtime continues a session under the task its log recorded, so such a session has no goal
    /// to be opened under, and opening it with the command line's goal would spend a model call on a
    /// session nobody asked that of.
    fn run_for(
        &self,
        session: SessionID,
    ) -> Result<Run, Error> {
        self.taskless(session)?;

        Ok(Run {
            session: session.as_uuid().to_string(),
            ..self.run.clone()
        })
    }

    /// Refuses a session whose log holds a history and no task in it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Untasked`] for such a session, and the log's failure when it cannot be read.
    fn taskless(
        &self,
        session: SessionID,
    ) -> Result<(), Error> {
        // The listing is the kernel's own reading of the log, so what a session's history holds stays
        // one answer from one source.
        let summary = mokmok_kernel::sessions(&self.config)?
            .into_iter()
            .find(|summary| summary.session == session);

        match summary {
            // The log holds nothing for the session, so the command line opens it.
            None => Ok(()),
            // The kernel reads both the goal and the checkout from the one task record a session
            // wrote, so the goal stands for the task being there at all.
            Some(summary) if summary.goal.is_some() => Ok(()),
            Some(_) => Err(Error::Untasked(session)),
        }
    }
}

impl Roster for Sessions {
    type Engine = Session;

    fn listed(&self) -> Result<Vec<Listing>, Error> {
        Ok(mokmok_kernel::sessions(&self.config)?
            .into_iter()
            .map(|summary| Listing {
                session: summary.session,
                phase: summary.phase,
                goal: summary.goal,
                checkout: summary.checkout,
            })
            .collect())
    }

    fn resume(
        &self,
        listing: &Listing,
    ) -> Result<Self::Engine, Error> {
        let run = self.run_for(listing.session)?;

        Session::open(&self.config, &run, self.at)
    }

    fn create(
        &self,
        goal: &str,
    ) -> Result<Self::Engine, Error> {
        let session = mokmok_kernel::mint_session(self.at);
        let run = Run {
            goal: goal.to_owned(),
            ..self.run_for(session)?
        };

        Session::open(&self.config, &run, self.at)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use clap::Parser as _;
    use mokmok_agent_runtime::{
        Journal, Receipt, Record, SpeechRecord, Step, StepId, StepKey, turn_id,
    };
    use mokmok_kernel::agent::Ledger;
    use mokmok_kernel::{Agent, Args, Command, Config, Run};
    use mokmok_protocol::Moment;
    use mokmok_state_machine::{Fact, TurnFact, TurnNumber};

    use super::{Session, Sessions};
    use crate::Error;
    use crate::port::{Engine, Listing, Roster};
    use mokmok_protocol::SessionID;
    use mokmok_state_machine::Phase;

    const SESSION: &str = "018f2f1e-0000-7000-8000-000000000000";

    /// A directory of this case's own, removed when it ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mokmok-tui-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            // The checkout has to exist before it can be granted with `--allow-write`.
            std::fs::create_dir_all(path.join("checkout")).expect("the scratch checkout is made");

            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The first shell on `PATH`, which stands in for the confiner and the one allowed tool: no case
    /// here dispatches a command.
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

    /// The validated configuration and run one conversation names.
    fn config_and_run(
        scratch: &Scratch,
        script: &Path,
    ) -> (Config, Run) {
        let checkout = scratch.0.join("checkout");
        let args = vec![
            "mokmok-tui".to_owned(),
            "--data-dir".to_owned(),
            scratch.0.join("data").display().to_string(),
            "--confiner".to_owned(),
            shell().display().to_string(),
            "--allow-tool".to_owned(),
            shell().display().to_string(),
            "--allow-write".to_owned(),
            checkout.display().to_string(),
            "agent".to_owned(),
            "run".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--goal".to_owned(),
            "hold a conversation".to_owned(),
            "--checkout".to_owned(),
            checkout.display().to_string(),
            "--script".to_owned(),
            script.display().to_string(),
        ];
        let parsed = Args::try_parse_from(&args).expect("the command line parses");
        let config = Config::resolve(&parsed, None).expect("the configuration resolves");
        let Command::Agent(Agent::Run(run)) = parsed.command else {
            panic!("the command line named an agent run");
        };

        (config, run)
    }

    #[test]
    fn a_session_the_log_holds_nothing_for_is_opened_under_the_command_line_s_own_run() {
        let scratch = Scratch::new("resume-bare");
        let script = scratch.0.join("script.json");
        std::fs::write(&script, b"[]").expect("the script is written");
        let (config, run) = config_and_run(&scratch, &script);
        let checkout = scratch.0.join("checkout");
        let sessions = Sessions::new(config, run, Moment::from_millis(0));

        let listing = Listing {
            session: SessionID::from_uuid(uuid::Uuid::from_u128(0x11)),
            phase: Phase::Unopened,
            goal: None,
            checkout: None,
        };
        let resumed = sessions.resume(&listing).expect("the session opens");
        let task = resumed
            .fold()
            .expect("the fold reads")
            .task
            .expect("the run seeds the task the log does not hold");

        assert_eq!(task.goal, "hold a conversation");
        assert_eq!(task.setting.checkout, checkout);
    }

    #[test]
    fn a_session_the_log_holds_no_task_for_is_refused_every_way_in() {
        let scratch = Scratch::new("untasked");
        let script = scratch.0.join("script.json");
        std::fs::write(&script, b"[]").expect("the script is written");
        let (config, run) = config_and_run(&scratch, &script);
        let id = SessionID::from_uuid(uuid::Uuid::from_u128(0x21));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");

        // A session a `submit` wrote: its log holds session and turn facts, and no task.
        runtime.block_on(async {
            mokmok_kernel::submit(
                &config,
                &mokmok_kernel::Submit {
                    session: id.as_uuid().to_string(),
                    idempotency_key: uuid::Uuid::from_u128(0x22).to_string(),
                    program: PathBuf::from("/bin/sh"),
                    args: vec!["-c".to_owned(), "true".to_owned()],
                    artifacts: Vec::new(),
                    deadline_seconds: 30,
                },
                Moment::from_millis(0),
            )
            .await
            .expect("the submit lands");
        });

        let named = Sessions::new(
            config.clone(),
            Run {
                session: id.as_uuid().to_string(),
                ..run.clone()
            },
            Moment::from_millis(0),
        );
        let refused = named.start();

        assert!(
            matches!(refused, Err(Error::Untasked(refused)) if refused == id),
            "the command line naming it is refused",
        );

        let sessions = Sessions::new(config, run, Moment::from_millis(0));
        let listing = Listing {
            session: id,
            phase: Phase::Accepting,
            goal: None,
            checkout: None,
        };
        let refused = sessions.resume(&listing);

        assert!(
            matches!(refused, Err(Error::Untasked(refused)) if refused == id),
            "and so is choosing it from the listing",
        );
    }

    #[test]
    fn a_session_that_recorded_a_task_is_opened_under_the_goal_the_log_holds() {
        let scratch = Scratch::new("resume-recorded");
        let script = scratch.0.join("script.json");
        std::fs::write(&script, br#"[{"action": "final", "say": "done"}]"#)
            .expect("the script is written");
        let (config, run) = config_and_run(&scratch, &script);
        let checkout = scratch.0.join("checkout");
        let sessions = Sessions::new(config, run, Moment::from_millis(0));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");

        // The session's own first step is what writes its task, so the log is where the next opening
        // reads the goal and the checkout from.
        let mut created = sessions
            .create("count the quokkas")
            .expect("the session opens");
        let id = created.session();
        runtime.block_on(async {
            for _ in 0..4 {
                match created.step().await.expect("the step runs") {
                    mokmok_agent_runtime::Progress::Awaiting
                    | mokmok_agent_runtime::Progress::Terminal(_) => break,
                    _ => {},
                }
            }
        });

        let listing = Listing {
            session: id,
            phase: Phase::Accepting,
            goal: None,
            checkout: None,
        };
        let resumed = sessions.resume(&listing).expect("the session opens");
        let task = resumed
            .fold()
            .expect("the fold reads")
            .task
            .expect("the log holds the task");

        assert_eq!(task.goal, "count the quokkas");
        assert_eq!(task.setting.checkout, checkout);
    }

    #[test]
    fn a_created_session_runs_under_the_goal_the_human_typed() {
        let scratch = Scratch::new("create");
        let script = scratch.0.join("script.json");
        std::fs::write(&script, b"[]").expect("the script is written");
        let (config, run) = config_and_run(&scratch, &script);
        let checkout = scratch.0.join("checkout");
        let sessions = Sessions::new(config, run, Moment::from_millis(0));

        let created = sessions
            .create("count the quokkas")
            .expect("the session opens");
        let task = created
            .fold()
            .expect("the fold reads")
            .task
            .expect("the run seeds the task the log does not hold");

        assert_eq!(task.goal, "count the quokkas");
        assert_eq!(
            task.setting.checkout, checkout,
            "a session started here runs in the checkout the command line named",
        );
    }

    #[test]
    fn a_created_session_is_one_nothing_has_opened_yet() {
        let scratch = Scratch::new("create-identity");
        let script = scratch.0.join("script.json");
        std::fs::write(&script, b"[]").expect("the script is written");
        let (config, run) = config_and_run(&scratch, &script);
        let sessions = Sessions::new(config, run, Moment::from_millis(0));

        let first = sessions.create("one").expect("the session opens");
        let second = sessions.create("two").expect("the session opens");

        assert_ne!(
            first.session(),
            second.session(),
            "two sessions minted from one clock reading are two identities",
        );
        assert_eq!(
            second
                .fold()
                .expect("the fold reads")
                .task
                .map(|task| task.goal),
            Some("two".to_owned()),
            "each session carries the goal it was named with",
        );
    }

    #[test]
    fn a_listed_session_maps_onto_what_the_kernel_holds_of_it() {
        let scratch = Scratch::new("listed");
        let script = scratch.0.join("script.json");
        let answers = serde_json::to_vec(&serde_json::json!([
            {"action": "final", "say": "the reply"}
        ]))
        .expect("the script encodes");
        std::fs::write(&script, answers).expect("the script is written");
        let (config, run) = config_and_run(&scratch, &script);
        let checkout = scratch.0.join("checkout");
        let id = SessionID::from_uuid(
            SESSION
                .parse::<uuid::Uuid>()
                .expect("the session is a uuid"),
        );
        let sessions = Sessions::new(config, run, Moment::from_millis(0));

        assert_eq!(
            sessions.listed().expect("the log is read"),
            Vec::new(),
            "a log nothing has committed to holds no session",
        );

        // One turn, so the session has a task record and a settled answer the listing reads.
        let mut engine = sessions.start().expect("the session opens");
        engine.speak("hello").expect("the session takes the line");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async {
            for _ in 0..8 {
                match engine.step().await.expect("the step runs") {
                    mokmok_agent_runtime::Progress::Awaiting
                    | mokmok_agent_runtime::Progress::Terminal(_) => break,
                    _ => {},
                }
            }
        });

        let listed = sessions.listed().expect("the log is read");

        assert_eq!(
            listed,
            vec![Listing {
                session: id,
                phase: Phase::Accepting,
                goal: Some("hold a conversation".to_owned()),
                checkout: Some(checkout),
            }],
        );
    }

    #[test]
    fn reopen_reads_a_step_another_writer_committed() {
        let scratch = Scratch::new("reopen");
        let script = scratch.0.join("script.json");
        std::fs::write(&script, b"[]").expect("the script is written");
        let (config, run) = config_and_run(&scratch, &script);

        let mut session =
            Session::open(&config, &run, Moment::from_millis(0)).expect("the session opens");
        let id = session.session();
        let turn = turn_id(id, 1);
        let line = "a line from another writer";

        // A resume reads a step the engine's own fold has not seen, so it takes a second writer.
        let mut ledger =
            Ledger::open(&config, id, None, Moment::from_millis(0)).expect("the log opens");
        let step = Step::new(
            StepId::derive(id, StepKey::Speech { turn: 1 }),
            id,
            vec![
                Record::Session {
                    fact: Fact::SessionOpened,
                },
                Record::Turn {
                    turn,
                    fact: TurnFact::TurnOpened {
                        number: TurnNumber::new(1),
                    },
                },
                Record::Speech(SpeechRecord {
                    turn,
                    text: line.to_owned(),
                }),
            ],
        );

        assert_eq!(
            ledger.append(step).expect("the step commits"),
            Receipt::Written,
        );
        assert_eq!(session.fold().expect("the fold reads").speech(turn), None);

        session.reopen().expect("the session re-opens");

        assert_eq!(
            session.fold().expect("the fold reads").speech(turn),
            Some(line),
        );
    }
}
