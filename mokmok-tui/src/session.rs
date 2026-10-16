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
    /// Returns the errors of opening the session's world.
    pub fn start(&self) -> Result<Session, Error> {
        Session::open(&self.config, &self.run, self.at)
    }

    /// The command line's own run, with a session's identity and, when it has one, the goal and the
    /// checkout that session recorded.
    ///
    /// A session that recorded a task is held to it: the fold prefers the recorded task for the
    /// prompt the model is handed, so the run only has to name the session and stay inside the
    /// grants the operator passed.
    fn run_for(
        &self,
        session: SessionID,
        listing: Option<&Listing>,
    ) -> Run {
        let run = Run {
            session: session.as_uuid().to_string(),
            ..self.run.clone()
        };

        match listing {
            Some(listing) => Run {
                goal: listing.goal.clone().unwrap_or(run.goal),
                checkout: listing
                    .checkout
                    .clone()
                    .unwrap_or_else(|| run.checkout.clone()),
                ..run
            },
            None => run,
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
        let run = self.run_for(listing.session, Some(listing));

        Session::open(&self.config, &run, self.at)
    }

    fn create(
        &self,
        goal: &str,
    ) -> Result<Self::Engine, Error> {
        let session = mokmok_kernel::mint_session(self.at);
        let run = Run {
            goal: goal.to_owned(),
            ..self.run_for(session, None)
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

    use super::Session;
    use crate::port::Engine;

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
