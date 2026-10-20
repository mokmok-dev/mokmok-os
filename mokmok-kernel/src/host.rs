//! The daemon's session host: it owns every `Driver`, answers the bus, and publishes the record
//! stream a terminal folds.
//!
//! A client names a session, a goal, a checkout, and a line. It names no grant, no provider, and no
//! budget: those are the daemon's command line. A command is acknowledged only after its effect is
//! durable, so a crash between the write and the ack replays the command instead of losing it.

use std::collections::HashMap;

use mokmok_agent_runtime::{MAX_SPEECH_BYTES, Record, Task, Transcript};
use mokmok_event_bus::{Client, Cursor, Payload, Subscription};
use mokmok_protocol::{AgentID, CommandID, Moment, SessionID};
use mokmok_session_wire::{
    Command, DaemonEvent, Outcome, PROTOCOL, Refusal, Reply, RosterEntry, SessionEvent,
    command_topic, daemon_subscriber, daemon_topic, history_chunks, roster_chunks, session_topic,
};
use mokmok_state_machine::Phase;
use tokio::sync::{mpsc, oneshot};

use crate::config::{Config, SessionDefaults};
use crate::db::Db;
use crate::error::Error;
use crate::host::slot::{Command as SlotCommand, Event as SlotEvent, Handle, spawn, working};
use crate::roster::sessions;

mod slot;

/// What the daemon remembers about a session it created but whose log holds no record yet.
#[derive(Clone, Debug)]
struct Created {
    goal: String,
    checkout: std::path::PathBuf,
    task: mokmok_agent_runtime::TaskRecord,
}

/// What one session's thread last reported about its fold.
#[derive(Clone, Copy, Debug)]
struct State {
    phase: Phase,
    working: bool,
}

/// The daemon's bus face and session owner.
pub struct Host {
    config: Config,
    defaults: SessionDefaults,
    at: Moment,
    daemon: AgentID,
    client: Client,
    commands: mpsc::UnboundedReceiver<Incoming>,
    slots: HashMap<SessionID, Handle>,
    states: HashMap<SessionID, State>,
    created: HashMap<SessionID, Created>,
    roster: Vec<RosterEntry>,
    revision: u64,
    events: mpsc::UnboundedSender<SlotEvent>,
    received: mpsc::UnboundedReceiver<SlotEvent>,
}

impl Host {
    /// Subscribes the daemon's command checkpoint and prepares the roster.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Bus`] when the daemon cannot subscribe.
    pub async fn start(
        config: Config,
        defaults: SessionDefaults,
        at: Moment,
        daemon: AgentID,
        client: Client,
    ) -> Result<Self, Error> {
        let subscription = client
            .subscribe(command_topic(), daemon_subscriber(&config.data_dir))
            .await
            .map_err(Error::Bus)?;
        let (events, received) = mpsc::unbounded_channel();
        let (incoming, commands) = mpsc::unbounded_channel();
        tokio::spawn(read_commands(subscription, incoming));

        Ok(Self {
            config,
            defaults,
            at,
            daemon,
            client,
            commands,
            slots: HashMap::new(),
            states: HashMap::new(),
            created: HashMap::new(),
            roster: Vec::new(),
            revision: 0,
            events,
            received,
        })
    }

    /// Answers commands and session reports until the daemon stops.
    ///
    /// # Errors
    ///
    /// Returns the bus's failure when a pull, an ack, or a publish fails, and the log's when a
    /// history or a roster cannot be read.
    pub async fn run(mut self) -> Result<(), Error> {
        self.refresh_roster().await?;
        self.publish_roster().await?;

        loop {
            tokio::select! {
                Some(incoming) = self.commands.recv() => {
                    self.handle(incoming.command).await?;
                    // The reader acks the command only now, after the daemon made its effect
                    // durable. A crash before this point replays the command; a crash after it
                    // loses nothing.
                    let _ = incoming.ack.send(());
                },
                Some(event) = self.received.recv() => self.report(event).await?,
            }
        }
    }

    async fn handle(
        &mut self,
        command: Command,
    ) -> Result<(), Error> {
        match command {
            Command::Hello { request, protocol } => self.hello(request, protocol).await,
            Command::List { .. } => {
                self.refresh_roster().await?;
                self.publish_roster().await
            },
            Command::Create {
                request,
                session,
                goal,
                checkout,
            } => self.create(request, session, goal, checkout).await,
            Command::Open { request, session } => self.open(request, session).await,
            Command::Speak {
                request,
                session,
                line,
            } => self.speak(request, session, line).await,
            Command::History { request, session } => self.history(request, session).await,
        }
    }

    async fn hello(
        &self,
        request: CommandID,
        protocol: u16,
    ) -> Result<(), Error> {
        if protocol != PROTOCOL {
            return self
                .refuse(request, Refusal::Unsupported { protocol })
                .await;
        }

        self.reply(Reply::Welcome {
            request,
            daemon: self.daemon,
            protocol,
        })
        .await
    }

    async fn create(
        &mut self,
        request: CommandID,
        session: SessionID,
        goal: String,
        checkout: std::path::PathBuf,
    ) -> Result<(), Error> {
        if goal.trim().is_empty() {
            return self
                .refuse(
                    request,
                    Refusal::Malformed {
                        detail: "the goal is empty".to_owned(),
                    },
                )
                .await;
        }
        if goal.len() > MAX_SPEECH_BYTES {
            return self
                .refuse(
                    request,
                    Refusal::TooMuch {
                        limit: MAX_SPEECH_BYTES,
                        found: goal.len(),
                    },
                )
                .await;
        }
        if !self.granted(&checkout) {
            return self
                .refuse(request, Refusal::CheckoutUngranted { checkout })
                .await;
        }

        let known = match self.known(session).await {
            Ok(known) => known,
            Err(error) => {
                return self
                    .refuse(
                        request,
                        Refusal::Unavailable {
                            detail: error.to_string(),
                        },
                    )
                    .await;
            },
        };
        if let Some(task) = &known.task {
            if task.goal != goal {
                return self.refuse(request, Refusal::GoalDiffers { session }).await;
            }

            return self.accept(request, session).await;
        }
        if known.has_facts {
            return self.refuse(request, Refusal::Untasked { session }).await;
        }
        if let Some(created) = self.created.get(&session)
            && created.goal != goal
        {
            return self.refuse(request, Refusal::GoalDiffers { session }).await;
        }

        let run = self.defaults.run(
            session.as_uuid().to_string(),
            goal.clone(),
            checkout.clone(),
        );
        let task = match run.task(&self.config) {
            Ok(task) => task,
            Err(error) => {
                return self
                    .refuse(
                        request,
                        Refusal::Unavailable {
                            detail: error.to_string(),
                        },
                    )
                    .await;
            },
        };
        let record = mokmok_agent_runtime::TaskRecord { task };
        self.created.insert(
            session,
            Created {
                goal,
                checkout,
                task: record.clone(),
            },
        );
        self.send_session(
            session,
            SessionEvent::Seeded {
                session,
                task: record,
            },
        )
        .await?;
        self.accept(request, session).await?;
        self.refresh_roster().await?;
        self.publish_roster().await
    }

    async fn open(
        &mut self,
        request: CommandID,
        session: SessionID,
    ) -> Result<(), Error> {
        match self.ensure(session, false).await {
            Ok(()) => self.accept(request, session).await?,
            Err(reason) => return self.refuse(request, reason).await,
        }
        self.refresh_roster().await?;
        self.publish_roster().await
    }

    async fn speak(
        &mut self,
        request: CommandID,
        session: SessionID,
        line: String,
    ) -> Result<(), Error> {
        if line.trim().is_empty() {
            return self
                .refuse(
                    request,
                    Refusal::Malformed {
                        detail: "an empty line opens no turn".to_owned(),
                    },
                )
                .await;
        }
        if line.len() > MAX_SPEECH_BYTES {
            return self
                .refuse(
                    request,
                    Refusal::TooMuch {
                        limit: MAX_SPEECH_BYTES,
                        found: line.len(),
                    },
                )
                .await;
        }
        if let Some(state) = self.states.get(&session) {
            if state.working {
                return self.refuse(request, Refusal::NotAwaiting { session }).await;
            }
            if matches!(state.phase, Phase::Closed(_)) {
                return self.refuse(request, Refusal::Closed { session }).await;
            }
        }
        if let Err(reason) = self.ensure(session, true).await {
            return self.refuse(request, reason).await;
        }

        let Some(handle) = self.slots.get(&session) else {
            return self
                .refuse(request, Refusal::UnknownSession { session })
                .await;
        };
        let (reply, receipt) = oneshot::channel();
        if handle
            .commands
            .send(SlotCommand::Speak {
                request,
                line,
                reply,
            })
            .is_err()
        {
            self.slots.remove(&session);
            return self
                .refuse(request, Refusal::UnknownSession { session })
                .await;
        }

        match receipt.await {
            Ok(Ok(_)) => self.accept(request, session).await?,
            Ok(Err(reason)) => self.refuse(request, reason).await?,
            Err(_) => {
                return self
                    .refuse(
                        request,
                        Refusal::Unavailable {
                            detail: "the session's thread stopped".to_owned(),
                        },
                    )
                    .await;
            },
        }
        self.refresh_roster().await?;
        self.publish_roster().await
    }

    async fn history(
        &mut self,
        request: CommandID,
        session: SessionID,
    ) -> Result<(), Error> {
        let Some((records, seed)) = self.history_of(session).await? else {
            return self
                .send_session(
                    session,
                    SessionEvent::HistoryRefused {
                        request,
                        session,
                        reason: Refusal::UnknownSession { session },
                    },
                )
                .await;
        };
        let chunks = history_chunks(session, request, seed, records).map_err(Error::Wire)?;
        for chunk in chunks {
            self.send_session(session, chunk).await?;
        }

        Ok(())
    }

    /// Opens the session's thread when it owes a driver.
    ///
    /// A session the daemon created but whose log holds no record can be listed and opened
    /// without a driver; `Speak` is the first command that needs one, so `open_created` is false
    /// for `Open` and true for `Speak`.
    async fn ensure(
        &mut self,
        session: SessionID,
        open_created: bool,
    ) -> Result<(), Refusal> {
        if self.slots.contains_key(&session) {
            return Ok(());
        }

        let known = self
            .known(session)
            .await
            .map_err(|error| Refusal::Unavailable {
                detail: error.to_string(),
            })?;
        let (goal, checkout) = if let Some(task) = &known.task {
            (task.goal.clone(), task.setting.checkout.clone())
        } else if known.has_facts {
            return Err(Refusal::Untasked { session });
        } else if let Some(created) = self.created.get(&session) {
            if !open_created {
                return Ok(());
            }
            (created.goal.clone(), created.checkout.clone())
        } else {
            return Err(Refusal::UnknownSession { session });
        };

        self.states.insert(
            session,
            State {
                phase: known.phase,
                working: known.working,
            },
        );
        let run = self
            .defaults
            .run(session.as_uuid().to_string(), goal, checkout);
        let handle =
            spawn(self.config.clone(), session, self.at, self.events.clone()).map_err(|error| {
                Refusal::Unavailable {
                    detail: error.to_string(),
                }
            })?;
        let (reply, opened) = oneshot::channel();
        if handle
            .commands
            .send(SlotCommand::Open { run, reply })
            .is_err()
        {
            return Err(Refusal::Unavailable {
                detail: "the session's thread stopped".to_owned(),
            });
        }

        match opened.await {
            Ok(Ok(())) => {
                self.slots.insert(session, handle);

                Ok(())
            },
            Ok(Err(reason)) => Err(reason),
            Err(_) => Err(Refusal::Unavailable {
                detail: "the session's thread stopped".to_owned(),
            }),
        }
    }

    async fn report(
        &mut self,
        event: SlotEvent,
    ) -> Result<(), Error> {
        match event {
            SlotEvent::State {
                session,
                phase,
                working,
            } => {
                self.states.insert(session, State { phase, working });
                self.refresh_roster().await?;
                self.publish_roster().await?;
            },
            SlotEvent::Fragment { session, text } => {
                self.send_daemon(DaemonEvent::Progress { session, text })
                    .await?;
            },
            SlotEvent::Trouble { session, detail } => {
                self.send_session(session, SessionEvent::Trouble { session, detail })
                    .await?;
            },
            SlotEvent::Stopped { session } => {
                self.slots.remove(&session);
                self.states.remove(&session);
            },
        }

        Ok(())
    }

    /// Reads one session's log and folds it, without a `Driver`.
    async fn known(
        &self,
        session: SessionID,
    ) -> Result<Known, Error> {
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let db = Db::open(&config.events)?;
            let log = db.log(session)?;
            let task = log.ordered.iter().find_map(|record| match record {
                Record::Task(record) => Some(record.task.clone()),
                _ => None,
            });
            let fold = Transcript::replay(session, &log.ordered).map_err(Error::Fold)?;

            Ok(Known {
                has_facts: !log.session.is_empty(),
                task,
                phase: fold.session.phase(),
                working: working(&fold),
            })
        })
        .await
        .map_err(join)?
    }

    async fn history_of(
        &self,
        session: SessionID,
    ) -> Result<Option<(Vec<Record>, Option<mokmok_agent_runtime::TaskRecord>)>, Error> {
        let config = self.config.clone();
        let seed = self
            .created
            .get(&session)
            .map(|created| created.task.clone());
        tokio::task::spawn_blocking(move || {
            let db = Db::open(&config.events)?;
            let log = db.log(session)?;
            if log.session.is_empty() && seed.is_none() {
                return Ok(None);
            }

            Ok(Some((log.ordered, seed)))
        })
        .await
        .map_err(join)?
    }

    async fn refresh_roster(&mut self) -> Result<(), Error> {
        let config = self.config.clone();
        let summaries = tokio::task::spawn_blocking(move || sessions(&config))
            .await
            .map_err(join)??;

        let mut roster: Vec<RosterEntry> = summaries
            .into_iter()
            .map(|summary| RosterEntry {
                session: summary.session,
                phase: summary.phase,
                goal: summary.goal,
                checkout: summary.checkout,
                turns: summary.turns,
            })
            .collect();
        for (session, created) in &self.created {
            if !roster.iter().any(|entry| entry.session == *session) {
                roster.push(RosterEntry {
                    session: *session,
                    phase: Phase::Unopened,
                    goal: Some(created.goal.clone()),
                    checkout: Some(created.checkout.clone()),
                    turns: 0,
                });
            }
        }

        self.roster = roster;
        self.revision = self.revision.saturating_add(1);

        Ok(())
    }

    async fn publish_roster(&self) -> Result<(), Error> {
        let chunks =
            roster_chunks(self.daemon, self.revision, self.roster.clone()).map_err(Error::Wire)?;
        for chunk in chunks {
            self.send_daemon(chunk).await?;
        }

        Ok(())
    }

    async fn reply(
        &self,
        reply: Reply,
    ) -> Result<(), Error> {
        let payload = Payload::json(&reply).map_err(Error::BusPayload)?;
        self.client
            .publish(command_topic(), payload)
            .await
            .map(|_| ())
            .map_err(Error::Bus)
    }

    async fn send_session(
        &self,
        session: SessionID,
        event: SessionEvent,
    ) -> Result<(), Error> {
        let payload = Payload::json(&event).map_err(Error::BusPayload)?;
        self.client
            .publish(session_topic(session), payload)
            .await
            .map(|_| ())
            .map_err(Error::Bus)
    }

    async fn send_daemon(
        &self,
        event: DaemonEvent,
    ) -> Result<(), Error> {
        let payload = Payload::json(&event).map_err(Error::BusPayload)?;
        self.client
            .publish(daemon_topic(self.daemon), payload)
            .await
            .map(|_| ())
            .map_err(Error::Bus)
    }

    async fn accept(
        &self,
        request: CommandID,
        session: SessionID,
    ) -> Result<(), Error> {
        self.reply(Reply::Ack {
            request,
            outcome: Outcome::Accepted { session },
        })
        .await
    }

    async fn refuse(
        &self,
        request: CommandID,
        reason: Refusal,
    ) -> Result<(), Error> {
        self.reply(Reply::Ack {
            request,
            outcome: Outcome::Refused { reason },
        })
        .await
    }

    fn granted(
        &self,
        checkout: &std::path::Path,
    ) -> bool {
        checkout.is_dir()
            && self
                .config
                .allow_write
                .iter()
                .any(|grant| checkout.starts_with(grant))
    }
}

/// One command the reader handed the host, and the signal that lets the reader ack it.
struct Incoming {
    command: Command,
    ack: oneshot::Sender<()>,
}

/// Reads the daemon's command checkpoint and hands one command at a time to the host.
///
/// A `Subscription::next` future cannot be cancelled and asked again: the server keeps the parked
/// pull and refuses the second one. The reader therefore owns the subscription and always awaits a
/// pull to completion, and it acks each command only after the host says its effect is durable.
async fn read_commands(
    mut subscription: Subscription,
    commands: mpsc::UnboundedSender<Incoming>,
) {
    loop {
        let Ok(batch) = subscription.next().await else {
            return;
        };

        for event in batch.events() {
            let cursor: Cursor = batch.after_event(event);
            let Ok(command) = event.payload().decode::<Command>() else {
                if subscription.ack(cursor).await.is_err() {
                    return;
                }
                continue;
            };
            let (ack, acked) = oneshot::channel();
            if commands.send(Incoming { command, ack }).is_err() {
                return;
            }
            if acked.await.is_err() {
                return;
            }
            if subscription.ack(cursor).await.is_err() {
                return;
            }
        }

        // A lapse means commands were evicted before this daemon read them. Acking the floor is the
        // only cursor left; a client that still needs its line re-publishes it.
        if let Some(lapsed) = batch.lapsed() {
            let cursor = Cursor::new(lapsed.held().epoch(), lapsed.from());
            if subscription.ack(cursor).await.is_err() {
                return;
            }
        }
    }
}

/// The fold a command boundary reads without opening a driver.
struct Known {
    has_facts: bool,
    task: Option<Task>,
    phase: Phase,
    working: bool,
}

fn join(error: tokio::task::JoinError) -> Error {
    Error::Io(std::io::Error::other(error))
}
