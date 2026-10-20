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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use mokmok_agent_runtime::{MAX_SPEECH_BYTES, Record, TaskRecord};
    use mokmok_event_bus::{
        Client, Config as BusConfig, Payload, SubscriberId, Subscription, Topic,
    };
    use mokmok_event_store::{CommandId, Expected};
    use mokmok_protocol::{AgentID, CommandID, Moment, SessionID};
    use mokmok_session_wire::{
        Command, DaemonEvent, Outcome, PROTOCOL, Refusal, Reply, RosterChunk, RosterEntry,
        SessionEvent, command_topic, daemon_topic, session_topic,
    };
    use mokmok_state_machine::{Fact, Phase};
    use serde_json::json;
    use uuid::Uuid;

    use super::Host;
    use crate::config::{Config, ProviderKind, ProviderSettings, SessionDefaults};
    use crate::db::{Db, Entry, Log};
    use crate::error::Error;
    use crate::serve::Relay;

    /// The longest one test waits for one delivery or one turn.
    const WAIT: Duration = Duration::from_secs(1);

    const GOAL: &str = "port the parser";
    const OTHER_GOAL: &str = "write the release notes";

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mokmok-host-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            for directory in ["data/artifacts", "data/sandbox", "checkout"] {
                std::fs::create_dir_all(path.join(directory)).unwrap();
            }

            Self(path)
        }

        fn data(&self) -> PathBuf {
            self.0.join("data")
        }

        fn checkout(&self) -> PathBuf {
            self.0.join("checkout")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn config(
        scratch: &Scratch,
        script: Option<&Path>,
    ) -> Config {
        let program = std::env::current_exe().unwrap();

        Config {
            data_dir: scratch.data(),
            events: scratch.data().join("events.sqlite"),
            artifacts: scratch.data().join("artifacts"),
            sandbox_root: scratch.data().join("sandbox"),
            allow_tool: vec![program.clone()],
            allow_path: Vec::new(),
            allow_write: vec![scratch.checkout()],
            search_root: Vec::new(),
            search_index: scratch.data().join("search"),
            skill_root: Vec::new(),
            tool_env: Vec::new(),
            confiner: program,
            crash_after: None,
            provider: ProviderSettings {
                kind: ProviderKind::Scripted,
                script: script.map(Path::to_path_buf),
                ..ProviderSettings::default()
            },
        }
    }

    fn defaults() -> SessionDefaults {
        SessionDefaults {
            max_turns: 6,
            token_budget: 0,
            context_bytes: 0,
            observation_bytes: 4096,
            context_turns: 16,
            tool_deadline_seconds: 30,
            steps: 64,
        }
    }

    const fn session_of(seed: u128) -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(seed))
    }

    const fn request(seed: u128) -> CommandID {
        CommandID::from_uuid(Uuid::from_u128(seed))
    }

    fn accepted(
        request: CommandID,
        session: SessionID,
    ) -> Reply {
        Reply::Ack {
            request,
            outcome: Outcome::Accepted { session },
        }
    }

    fn refused_reason(reply: &Reply) -> &Refusal {
        match reply {
            Reply::Ack {
                outcome: Outcome::Refused { reason },
                ..
            } => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    fn seed_facts(
        config: &Config,
        session: SessionID,
    ) {
        let mut db = Db::open(&config.events).unwrap();
        db.append(
            CommandId::new(),
            Db::stream(session),
            Expected::New,
            &[Entry::Session(Fact::SessionOpened)],
            Vec::new(),
        )
        .unwrap();
    }

    /// Writes a task and a close into `session`'s log, so it folds closed with no driver.
    fn seed_closed(
        config: &Config,
        scratch: &Scratch,
        session: SessionID,
    ) {
        let task = defaults()
            .run(
                session.as_uuid().to_string(),
                GOAL.to_owned(),
                scratch.checkout(),
            )
            .task(config)
            .unwrap();
        let mut db = Db::open(&config.events).unwrap();
        db.append(
            CommandId::new(),
            Db::stream(session),
            Expected::New,
            &[
                Entry::Session(Fact::SessionOpened),
                Entry::Agent(Record::Task(TaskRecord { task })),
                Entry::Session(Fact::SessionClosed),
            ],
            Vec::new(),
        )
        .unwrap();
    }

    async fn wait_for_socket(socket: &Path) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !socket.exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the bus bound {} within {WAIT:?}",
                socket.display(),
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// One daemon on a bus of its own, with a client a test drives it through.
    struct Bench {
        scratch: Scratch,
        config: Config,
        socket: PathBuf,
        client: Client,
        daemon: AgentID,
        commands: Subscription,
        host: tokio::task::JoinHandle<Result<(), Error>>,
        bus: tokio::task::JoinHandle<Result<(), mokmok_event_bus::ServeError>>,
    }

    impl Bench {
        async fn start(
            name: &str,
            answers: Option<&[serde_json::Value]>,
        ) -> Self {
            let scratch = Scratch::new(name);
            let script = answers.map(|answers| {
                let path = scratch.0.join("script.json");
                std::fs::write(&path, serde_json::to_vec(answers).unwrap()).unwrap();

                path
            });
            let config = config(&scratch, script.as_deref());
            let socket = scratch.0.join("bus.sock");
            let bus = tokio::spawn(mokmok_event_bus::serve(
                BusConfig {
                    path: socket.clone(),
                    store: scratch.0.join("bus.sqlite"),
                    ..BusConfig::default()
                },
                std::future::pending::<()>(),
            ));

            wait_for_socket(&socket).await;

            let client = Client::connect(&socket).await.unwrap();
            let daemon = AgentID::from_uuid(Uuid::from_u128(0xda_e0));
            let host = Host::start(
                config.clone(),
                defaults(),
                Moment::from_millis(0),
                daemon,
                client.clone(),
            )
            .await
            .unwrap();
            let host = tokio::spawn(host.run());
            let commands = client
                .subscribe(Topic::Global, SubscriberId::new())
                .await
                .unwrap();

            Self {
                scratch,
                config,
                socket,
                client,
                daemon,
                commands,
                host,
                bus,
            }
        }

        async fn send(
            &self,
            command: &Command,
        ) {
            self.client
                .publish(command_topic(), Payload::json(command).unwrap())
                .await
                .unwrap();
        }

        async fn asked(
            &mut self,
            command: &Command,
        ) -> Reply {
            self.send(command).await;
            self.reply(command.request()).await
        }

        async fn reply(
            &mut self,
            request: CommandID,
        ) -> Reply {
            loop {
                let batch = tokio::time::timeout(WAIT, self.commands.next())
                    .await
                    .expect("a reply arrives within one second")
                    .expect("the command subscription reads");
                for event in batch.events() {
                    let Ok(reply) = event.payload().decode::<Reply>() else {
                        continue;
                    };
                    if reply_request(&reply) == request {
                        self.commands.ack(batch.after_event(event)).await.unwrap();

                        return reply;
                    }
                }
                self.commands.ack(batch.after()).await.unwrap();
            }
        }

        async fn watch(
            &self,
            topic: Topic,
        ) -> Subscription {
            self.client
                .subscribe(topic, SubscriberId::new())
                .await
                .unwrap()
        }

        async fn session_event(
            &self,
            topic: Topic,
            want: impl Fn(&SessionEvent) -> bool,
        ) -> SessionEvent {
            let mut subscription = self.watch(topic).await;
            loop {
                let batch = tokio::time::timeout(WAIT, subscription.next())
                    .await
                    .expect("a session event arrives within one second")
                    .expect("the session subscription reads");
                for event in batch.events() {
                    let Ok(decoded) = event.payload().decode::<SessionEvent>() else {
                        continue;
                    };
                    if want(&decoded) {
                        return decoded;
                    }
                }
                subscription.ack(batch.after()).await.unwrap();
            }
        }

        async fn roster_chunk(
            &self,
            want: impl Fn(&RosterChunk) -> bool,
        ) -> RosterChunk {
            let mut subscription = self.watch(daemon_topic(self.daemon)).await;
            loop {
                let batch = tokio::time::timeout(WAIT, subscription.next())
                    .await
                    .expect("a roster arrives within one second")
                    .expect("the daemon subscription reads");
                for event in batch.events() {
                    let Ok(DaemonEvent::Roster(chunk)) = event.payload().decode::<DaemonEvent>()
                    else {
                        continue;
                    };
                    if want(&chunk) {
                        return chunk;
                    }
                }
                subscription.ack(batch.after()).await.unwrap();
            }
        }

        /// The newest roster the bus holds that `want` selects.
        async fn latest_roster(
            &self,
            want: impl Fn(&RosterChunk) -> bool,
        ) -> RosterChunk {
            let mut subscription = self.watch(daemon_topic(self.daemon)).await;
            let mut latest = None;
            loop {
                let batch = tokio::time::timeout(WAIT, subscription.next())
                    .await
                    .expect("a roster arrives within one second")
                    .expect("the daemon subscription reads");
                for event in batch.events() {
                    let Ok(DaemonEvent::Roster(chunk)) = event.payload().decode::<DaemonEvent>()
                    else {
                        continue;
                    };
                    if want(&chunk) {
                        latest = Some(chunk);
                    }
                }
                let caught_up = batch.after().seq().get() >= batch.window().next().get();
                subscription.ack(batch.after()).await.unwrap();
                if caught_up {
                    break;
                }
            }

            latest.expect("a roster the bus holds")
        }

        async fn roster_turns(
            &self,
            session: SessionID,
            turns: u64,
        ) -> RosterEntry {
            let chunk = self
                .roster_chunk(|chunk| {
                    chunk
                        .entries
                        .iter()
                        .any(|entry| entry.session == session && entry.turns == turns)
                })
                .await;

            chunk
                .entries
                .into_iter()
                .find(|entry| entry.session == session && entry.turns == turns)
                .unwrap()
        }

        /// Waits until the fragment holding `want` reaches the bus.
        async fn progress(
            &self,
            session: SessionID,
            want: &str,
        ) {
            let mut subscription = self.watch(daemon_topic(self.daemon)).await;
            loop {
                let batch = tokio::time::timeout(WAIT, subscription.next())
                    .await
                    .expect("a progress fragment arrives within one second")
                    .expect("the daemon subscription reads");
                for event in batch.events() {
                    let Ok(DaemonEvent::Progress {
                        session: named,
                        text,
                    }) = event.payload().decode::<DaemonEvent>()
                    else {
                        continue;
                    };
                    if named == session && text == want {
                        return;
                    }
                }
                subscription.ack(batch.after()).await.unwrap();
            }
        }

        /// Publishes every notice the log holds, the way a serving daemon's relay does.
        async fn publish_pending(&self) {
            let mut db = Db::open(&self.config.events).unwrap();
            let mut relay = Relay::new(&self.socket);
            let published = tokio::time::timeout(WAIT, relay.once(&mut db))
                .await
                .expect("the relay drains within one second")
                .unwrap();

            assert!(published > 0, "a settled turn left notices to publish");
        }

        fn log(
            &self,
            session: SessionID,
        ) -> Log {
            Db::open(&self.config.events).unwrap().log(session).unwrap()
        }
    }

    impl Drop for Bench {
        fn drop(&mut self) {
            self.host.abort();
            self.bus.abort();
        }
    }

    const fn reply_request(reply: &Reply) -> CommandID {
        match reply {
            Reply::Welcome { request, .. } | Reply::Ack { request, .. } => *request,
        }
    }

    #[tokio::test]
    async fn a_hello_names_the_daemon_and_refuses_another_protocol() {
        let mut bench = Bench::start("hello", None).await;

        let welcome = bench
            .asked(&Command::Hello {
                request: request(1),
                protocol: PROTOCOL,
            })
            .await;

        assert_eq!(
            welcome,
            Reply::Welcome {
                request: request(1),
                daemon: bench.daemon,
                protocol: PROTOCOL,
            },
        );

        let refused = bench
            .asked(&Command::Hello {
                request: request(2),
                protocol: PROTOCOL + 1,
            })
            .await;

        assert_eq!(
            refused,
            Reply::Ack {
                request: request(2),
                outcome: Outcome::Refused {
                    reason: Refusal::Unsupported {
                        protocol: PROTOCOL + 1,
                    },
                },
            },
        );
    }

    #[tokio::test]
    async fn create_seeds_a_granted_checkout_and_refuses_every_other() {
        let mut bench = Bench::start("create", None).await;
        let session = session_of(1);
        let checkout = bench.scratch.checkout();

        let created = bench
            .asked(&Command::Create {
                request: request(1),
                session,
                goal: GOAL.to_owned(),
                checkout: checkout.clone(),
            })
            .await;

        assert_eq!(created, accepted(request(1), session));

        let seeded = bench
            .session_event(session_topic(session), |event| {
                matches!(event, SessionEvent::Seeded { .. })
            })
            .await;
        let SessionEvent::Seeded {
            session: seeded_session,
            task,
        } = seeded
        else {
            unreachable!("the predicate selected a seed");
        };

        assert_eq!(seeded_session, session);
        assert_eq!(task.task.goal, GOAL);
        assert_eq!(task.task.setting.checkout, checkout);

        let outside = bench.scratch.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let refused = bench
            .asked(&Command::Create {
                request: request(2),
                session: session_of(2),
                goal: GOAL.to_owned(),
                checkout: outside.clone(),
            })
            .await;

        assert_eq!(
            refused_reason(&refused),
            &Refusal::CheckoutUngranted { checkout: outside },
        );

        let missing = checkout.join("missing");
        let refused = bench
            .asked(&Command::Create {
                request: request(3),
                session: session_of(3),
                goal: GOAL.to_owned(),
                checkout: missing.clone(),
            })
            .await;

        assert_eq!(
            refused_reason(&refused),
            &Refusal::CheckoutUngranted { checkout: missing },
        );

        for (seed, goal) in [(4, String::new()), (5, "  \n\t".to_owned())] {
            let refused = bench
                .asked(&Command::Create {
                    request: request(seed),
                    session: session_of(seed),
                    goal,
                    checkout: checkout.clone(),
                })
                .await;

            assert_eq!(
                refused_reason(&refused),
                &Refusal::Malformed {
                    detail: "the goal is empty".to_owned(),
                },
            );
        }
    }

    #[tokio::test]
    async fn create_holds_a_goal_at_the_speech_ceiling() {
        let mut bench = Bench::start("create-ceiling", None).await;
        let checkout = bench.scratch.checkout();

        let at_ceiling = bench
            .asked(&Command::Create {
                request: request(1),
                session: session_of(1),
                goal: "a".repeat(MAX_SPEECH_BYTES),
                checkout: checkout.clone(),
            })
            .await;

        assert_eq!(at_ceiling, accepted(request(1), session_of(1)));

        let refused = bench
            .asked(&Command::Create {
                request: request(2),
                session: session_of(2),
                goal: "a".repeat(MAX_SPEECH_BYTES + 1),
                checkout,
            })
            .await;

        assert_eq!(
            refused_reason(&refused),
            &Refusal::TooMuch {
                limit: MAX_SPEECH_BYTES,
                found: MAX_SPEECH_BYTES + 1,
            },
        );
    }

    #[tokio::test]
    async fn a_created_session_takes_its_goal_twice_and_refuses_a_different_one() {
        let mut bench = Bench::start("create-goal", None).await;
        let session = session_of(1);
        let checkout = bench.scratch.checkout();

        for (seed, goal) in [(1, GOAL), (2, GOAL)] {
            let reply = bench
                .asked(&Command::Create {
                    request: request(seed),
                    session,
                    goal: goal.to_owned(),
                    checkout: checkout.clone(),
                })
                .await;

            assert_eq!(reply, accepted(request(seed), session));
        }

        let refused = bench
            .asked(&Command::Create {
                request: request(3),
                session,
                goal: OTHER_GOAL.to_owned(),
                checkout,
            })
            .await;

        assert_eq!(refused_reason(&refused), &Refusal::GoalDiffers { session },);
    }

    #[tokio::test]
    async fn a_committed_task_accepts_its_own_goal_and_refuses_another() {
        let mut bench = Bench::start(
            "task-goal",
            Some(&[json!({"action": "final", "say": "done"})]),
        )
        .await;
        let session = session_of(1);
        let checkout = bench.scratch.checkout();

        assert_eq!(
            bench
                .asked(&Command::Create {
                    request: request(1),
                    session,
                    goal: GOAL.to_owned(),
                    checkout: checkout.clone(),
                })
                .await,
            accepted(request(1), session),
        );
        assert_eq!(
            bench
                .asked(&Command::Speak {
                    request: request(2),
                    session,
                    line: "hello".to_owned(),
                })
                .await,
            accepted(request(2), session),
        );
        bench.progress(session, "done").await;

        let same = bench
            .asked(&Command::Create {
                request: request(3),
                session,
                goal: GOAL.to_owned(),
                checkout: checkout.clone(),
            })
            .await;

        assert_eq!(same, accepted(request(3), session));

        let refused = bench
            .asked(&Command::Create {
                request: request(4),
                session,
                goal: OTHER_GOAL.to_owned(),
                checkout,
            })
            .await;

        assert_eq!(refused_reason(&refused), &Refusal::GoalDiffers { session },);
    }

    #[tokio::test]
    async fn create_refuses_a_session_that_holds_facts_without_a_task() {
        let mut bench = Bench::start("facts-only", None).await;
        let session = session_of(7);
        seed_facts(&bench.config, session);

        let refused = bench
            .asked(&Command::Create {
                request: request(1),
                session,
                goal: GOAL.to_owned(),
                checkout: bench.scratch.checkout(),
            })
            .await;

        assert_eq!(refused_reason(&refused), &Refusal::Untasked { session });
    }

    #[tokio::test]
    async fn open_reaches_a_created_session_without_opening_a_driver() {
        let mut bench = Bench::start("open", None).await;
        let session = session_of(1);
        let checkout = bench.scratch.checkout();

        assert_eq!(
            bench
                .asked(&Command::Create {
                    request: request(1),
                    session,
                    goal: GOAL.to_owned(),
                    checkout,
                })
                .await,
            accepted(request(1), session),
        );
        assert_eq!(
            bench
                .asked(&Command::Open {
                    request: request(2),
                    session,
                })
                .await,
            accepted(request(2), session),
        );

        // Speak is the first command that owes a driver, and the absent script is what makes the
        // difference between opening one here and not opening one at all visible.
        let refused = bench
            .asked(&Command::Speak {
                request: request(3),
                session,
                line: "hello".to_owned(),
            })
            .await;

        assert!(
            matches!(refused_reason(&refused), Refusal::Unavailable { .. }),
            "{refused:?}",
        );

        let unknown = session_of(2);
        let refused = bench
            .asked(&Command::Open {
                request: request(4),
                session: unknown,
            })
            .await;

        assert_eq!(
            refused_reason(&refused),
            &Refusal::UnknownSession { session: unknown },
        );
    }

    #[tokio::test]
    async fn speak_refuses_an_empty_line_and_a_line_past_the_ceiling() {
        let mut bench = Bench::start("speak-bounds", None).await;
        let session = session_of(9);

        for (seed, line) in [(1, String::new()), (2, " \t\n".to_owned())] {
            let refused = bench
                .asked(&Command::Speak {
                    request: request(seed),
                    session,
                    line,
                })
                .await;

            assert_eq!(
                refused_reason(&refused),
                &Refusal::Malformed {
                    detail: "an empty line opens no turn".to_owned(),
                },
            );
        }

        let at_ceiling = bench
            .asked(&Command::Speak {
                request: request(3),
                session,
                line: "a".repeat(MAX_SPEECH_BYTES),
            })
            .await;

        assert_eq!(
            refused_reason(&at_ceiling),
            &Refusal::UnknownSession { session },
        );

        let refused = bench
            .asked(&Command::Speak {
                request: request(4),
                session,
                line: "a".repeat(MAX_SPEECH_BYTES + 1),
            })
            .await;

        assert_eq!(
            refused_reason(&refused),
            &Refusal::TooMuch {
                limit: MAX_SPEECH_BYTES,
                found: MAX_SPEECH_BYTES + 1,
            },
        );
    }

    #[tokio::test]
    async fn a_spoken_line_commits_a_turn_and_the_next_line_opens_another() {
        let mut bench = Bench::start(
            "speak",
            Some(&[
                json!({"action": "find", "query": "quokka", "say": "looking"}),
                json!({"action": "final", "say": "done"}),
            ]),
        )
        .await;
        let session = session_of(1);
        let checkout = bench.scratch.checkout();

        assert_eq!(
            bench
                .asked(&Command::Create {
                    request: request(1),
                    session,
                    goal: GOAL.to_owned(),
                    checkout,
                })
                .await,
            accepted(request(1), session),
        );
        assert_eq!(
            bench
                .asked(&Command::Speak {
                    request: request(2),
                    session,
                    line: "hello".to_owned(),
                })
                .await,
            accepted(request(2), session),
        );

        bench.progress(session, "looking").await;
        assert_eq!(
            bench.roster_turns(session, 1).await.phase,
            Phase::Accepting,
            "the first line settled and the session waits for another",
        );

        bench.publish_pending().await;

        let find = bench
            .session_event(session_topic(session), |event| {
                matches!(event, SessionEvent::Commit(commit)
                    if commit.records.iter().any(|record| matches!(record, Record::Find(_))))
            })
            .await;
        let SessionEvent::Commit(find) = find else {
            unreachable!("the predicate selected a commit");
        };

        assert_eq!(find.session, session);
        assert!(
            find.records
                .iter()
                .any(|record| matches!(record, Record::Find(_))),
            "{:?}",
            find.records,
        );

        assert_eq!(
            bench
                .asked(&Command::Speak {
                    request: request(3),
                    session,
                    line: "again".to_owned(),
                })
                .await,
            accepted(request(3), session),
        );

        bench.progress(session, "done").await;
        assert_eq!(
            bench.roster_turns(session, 2).await.phase,
            Phase::Accepting,
            "the second line opened another turn",
        );
        bench.publish_pending().await;

        let answer = bench
            .session_event(session_topic(session), |event| {
                matches!(event, SessionEvent::Commit(commit)
                    if commit.records.iter().any(|record| matches!(record, Record::Answer(_))))
            })
            .await;
        let SessionEvent::Commit(answer) = answer else {
            unreachable!("the predicate selected a commit");
        };

        assert_eq!(answer.session, session);

        let log = bench.log(session);

        assert_eq!(
            log.ordered
                .iter()
                .filter(|record| matches!(record, Record::Speech(_)))
                .count(),
            2,
            "each line is its own speech record: {:?}",
            log.ordered,
        );
    }

    #[tokio::test]
    async fn a_closed_session_refuses_a_line_once_it_is_opened() {
        let mut bench = Bench::start("closed", Some(&[json!({"action": "final"})])).await;
        let session = session_of(1);
        seed_closed(&bench.config, &bench.scratch, session);

        assert_eq!(
            bench
                .asked(&Command::Open {
                    request: request(1),
                    session,
                })
                .await,
            accepted(request(1), session),
        );

        let refused = bench
            .asked(&Command::Speak {
                request: request(2),
                session,
                line: "again".to_owned(),
            })
            .await;

        assert_eq!(refused_reason(&refused), &Refusal::Closed { session });
    }

    #[tokio::test]
    async fn history_refuses_an_unknown_session_and_seeds_a_created_one() {
        let mut bench = Bench::start("history", None).await;
        let unknown = session_of(2);

        bench
            .send(&Command::History {
                request: request(1),
                session: unknown,
            })
            .await;

        let refused = bench
            .session_event(session_topic(unknown), |event| {
                matches!(event, SessionEvent::HistoryRefused { .. })
            })
            .await;

        assert_eq!(
            refused,
            SessionEvent::HistoryRefused {
                request: request(1),
                session: unknown,
                reason: Refusal::UnknownSession { session: unknown },
            },
        );

        let session = session_of(1);
        let checkout = bench.scratch.checkout();

        assert_eq!(
            bench
                .asked(&Command::Create {
                    request: request(2),
                    session,
                    goal: GOAL.to_owned(),
                    checkout,
                })
                .await,
            accepted(request(2), session),
        );

        let seeded = bench
            .session_event(session_topic(session), |event| {
                matches!(event, SessionEvent::Seeded { .. })
            })
            .await;
        let SessionEvent::Seeded { task, .. } = seeded else {
            unreachable!("the predicate selected a seed");
        };

        bench
            .send(&Command::History {
                request: request(3),
                session,
            })
            .await;

        let history = bench
            .session_event(session_topic(session), |event| {
                matches!(event, SessionEvent::History(_))
            })
            .await;
        let SessionEvent::History(chunk) = history else {
            unreachable!("the predicate selected a history chunk");
        };

        assert_eq!(chunk.request, request(3));
        assert_eq!(chunk.session, session);
        assert_eq!(chunk.seed, Some(task));
        assert!(chunk.records.is_empty(), "{:?}", chunk.records);
        assert!(chunk.last);
    }

    #[tokio::test]
    async fn history_answers_the_records_a_session_committed() {
        let mut bench = Bench::start(
            "history-records",
            Some(&[json!({"action": "final", "say": "done"})]),
        )
        .await;
        let session = session_of(1);
        let checkout = bench.scratch.checkout();

        assert_eq!(
            bench
                .asked(&Command::Create {
                    request: request(1),
                    session,
                    goal: GOAL.to_owned(),
                    checkout,
                })
                .await,
            accepted(request(1), session),
        );
        assert_eq!(
            bench
                .asked(&Command::Speak {
                    request: request(2),
                    session,
                    line: "hello".to_owned(),
                })
                .await,
            accepted(request(2), session),
        );
        bench.progress(session, "done").await;

        bench
            .send(&Command::History {
                request: request(3),
                session,
            })
            .await;

        let history = bench
            .session_event(session_topic(session), |event| {
                matches!(event, SessionEvent::History(_))
            })
            .await;
        let SessionEvent::History(chunk) = history else {
            unreachable!("the predicate selected a history chunk");
        };

        assert!(
            chunk
                .records
                .iter()
                .any(|record| matches!(record, Record::Task(_))),
            "{:?}",
            chunk.records,
        );
        assert!(
            chunk
                .records
                .iter()
                .any(|record| matches!(record, Record::Speech(_))),
            "{:?}",
            chunk.records,
        );
    }

    #[tokio::test]
    async fn list_names_a_created_session_once_after_its_task_commits() {
        let mut bench = Bench::start(
            "list-created",
            Some(&[json!({"action": "final", "say": "done"})]),
        )
        .await;
        let session = session_of(1);
        let other = session_of(2);
        let checkout = bench.scratch.checkout();

        assert_eq!(
            bench
                .asked(&Command::Create {
                    request: request(1),
                    session,
                    goal: GOAL.to_owned(),
                    checkout,
                })
                .await,
            accepted(request(1), session),
        );
        assert_eq!(
            bench
                .asked(&Command::Speak {
                    request: request(2),
                    session,
                    line: "hello".to_owned(),
                })
                .await,
            accepted(request(2), session),
        );
        bench.progress(session, "done").await;
        seed_facts(&bench.config, other);

        let before = bench.latest_roster(|_| true).await.revision;

        bench
            .send(&Command::List {
                request: request(3),
            })
            .await;

        let listed = bench
            .roster_chunk(|chunk| {
                chunk.revision > before
                    && chunk.entries.iter().any(|entry| entry.session == session)
            })
            .await;

        assert_eq!(
            listed
                .entries
                .iter()
                .filter(|entry| entry.session == session)
                .count(),
            1,
            "the session the log already lists is not added again: {:?}",
            listed.entries,
        );
    }

    #[tokio::test]
    async fn list_reports_a_created_session_before_its_first_record() {
        let mut bench = Bench::start("list", None).await;
        let session = session_of(1);
        let checkout = bench.scratch.checkout();
        seed_facts(&bench.config, session_of(2));

        assert_eq!(
            bench
                .asked(&Command::Create {
                    request: request(1),
                    session,
                    goal: GOAL.to_owned(),
                    checkout: checkout.clone(),
                })
                .await,
            accepted(request(1), session),
        );

        let created = bench
            .roster_chunk(|chunk| chunk.entries.iter().any(|entry| entry.session == session))
            .await;

        assert_eq!(created.daemon, bench.daemon);

        let entry = created
            .entries
            .into_iter()
            .find(|entry| entry.session == session)
            .unwrap();

        assert_eq!(entry.phase, Phase::Unopened);
        assert_eq!(entry.goal.as_deref(), Some(GOAL));
        assert_eq!(entry.checkout.as_deref(), Some(checkout.as_path()));
        assert_eq!(entry.turns, 0);

        bench
            .send(&Command::List {
                request: request(2),
            })
            .await;

        let listed = bench
            .roster_chunk(|chunk| chunk.revision > created.revision)
            .await;

        assert!(
            listed
                .entries
                .iter()
                .any(|entry| entry.session == session && entry.phase == Phase::Unopened),
            "{:?}",
            listed.entries,
        );
    }
}
