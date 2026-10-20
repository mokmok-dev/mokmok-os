//! The conversation as a bus client: one thread owns the socket, each session owns a fold of the
//! records the daemon publishes.
//!
//! The daemon is the only writer. A session's fold here is a cache rebuilt from the record stream,
//! and a history request is a full replacement when the retained window is shorter than the
//! session. A command is published once; the record stream says whether it landed, and the
//! acknowledgement only says the daemon read it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mokmok_agent_runtime::{Progress, Record, TaskRecord, Transcript};
use mokmok_event_bus::{Client, Payload, SubscriberId, Subscription, Topic};
use mokmok_protocol::{AgentID, CommandID, Moment, SessionID};
use mokmok_session_wire::{
    Command, DaemonEvent, HistoryChunk, Outcome, Reply, RosterEntry, SessionCommit, SessionEvent,
    command_topic, daemon_topic, daemon_view_subscriber, session_subscriber, session_topic,
};
use mokmok_state_machine::Phase;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use uuid::Uuid;

use crate::Error;
use crate::core::working;
use crate::port::{Engine, Link, Listing, Roster, Update};

const ATTACH: Duration = Duration::from_secs(5);
const READY: Duration = Duration::from_secs(5);

/// A command the screen thread asks the bus thread to publish or watch.
enum Request {
    Watch {
        session: SessionID,
        updates: Sender<Internal>,
        open: bool,
    },
    Create {
        request: CommandID,
        session: SessionID,
        goal: String,
        checkout: PathBuf,
    },
    Speak {
        request: CommandID,
        session: SessionID,
        line: String,
    },
    List,
    Stop,
}

/// One event the bus thread routes to one session's engine.
enum Internal {
    Commit(SessionCommit),
    History(HistoryChunk),
    Refused(mokmok_session_wire::Refusal),
    Seeded(TaskRecord),
    Trouble(String),
    Progress(String),
    Link(Link),
}

/// What the attach handshake learned before the screen exists.
struct Attached {
    roster: Vec<RosterEntry>,
}

/// What the roster pane reads, shared between the screen thread and the bus thread.
#[derive(Default)]
struct Shared {
    roster: Mutex<Vec<RosterEntry>>,
}

/// The bus-backed roster: one socket, one thread, one subscription per open session.
pub struct BusRoster {
    commands: UnboundedSender<Request>,
    shared: Arc<Shared>,
    at: Moment,
    checkout: PathBuf,
}

impl BusRoster {
    /// Connects, completes the handshake, and reads the first roster.
    ///
    /// Called before [`Terminal::open`](crate::Terminal), so it may wait; a refused or absent bus
    /// therefore leaves the screen alone.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Bus`] when the socket cannot be reached, or [`Error::BusThread`] when the
    /// handshake does not answer in time.
    pub fn attach(
        socket: &Path,
        at: Moment,
        checkout: PathBuf,
    ) -> Result<Self, Error> {
        let (ready, attached) = channel();
        let (commands, requests) = unbounded_channel();
        let shared = Arc::new(Shared::default());
        let nonce = Uuid::new_v4();
        let bus = Arc::clone(&shared);
        let socket = socket.to_path_buf();

        std::thread::Builder::new()
            .name("mokmok-bus".to_owned())
            .spawn(move || bus_thread(&socket, nonce, &bus, requests, &ready))
            .map_err(|_| Error::BusThread)?;

        match attached.recv_timeout(ATTACH) {
            Ok(Ok(attached)) => {
                let mut roster = shared.roster.lock().map_err(|_| Error::BusThread)?;
                roster.clone_from(&attached.roster);
                drop(roster);

                Ok(Self {
                    commands,
                    shared,
                    at,
                    checkout,
                })
            },
            Ok(Err(error)) => Err(error),
            Err(_) => Err(Error::BusThread),
        }
    }

    /// Opens the session the command line named and waits for its first fold.
    ///
    /// Called before the alternate screen, so an unknown or untasked session is an error the shell
    /// prints rather than a line the frame has to carry.
    ///
    /// # Errors
    ///
    /// Returns the daemon's refusal, or [`Error::BusThread`] when the fold never arrives.
    pub fn start(
        &self,
        session: SessionID,
    ) -> Result<RemoteEngine, Error> {
        self.engine(session, true).ready(READY)
    }

    fn engine(
        &self,
        session: SessionID,
        open: bool,
    ) -> RemoteEngine {
        let (updates, received) = channel();
        let _ = self.commands.send(Request::Watch {
            session,
            updates,
            open,
        });

        RemoteEngine::new(session, self.commands.clone(), received)
    }
}

impl Roster for BusRoster {
    type Engine = RemoteEngine;

    fn listed(&self) -> Result<Vec<Listing>, Error> {
        let _ = self.commands.send(Request::List);
        let roster = self.shared.roster.lock().map_err(|_| Error::BusThread)?;

        Ok(roster
            .iter()
            .map(|entry| Listing {
                session: entry.session,
                phase: entry.phase,
                goal: entry.goal.clone(),
                checkout: entry.checkout.clone(),
            })
            .collect())
    }

    fn resume(
        &self,
        listing: &Listing,
    ) -> Result<Self::Engine, Error> {
        Ok(self.engine(listing.session, true))
    }

    fn create(
        &self,
        goal: &str,
    ) -> Result<Self::Engine, Error> {
        let session = SessionID::mint(self.at);
        let engine = self.engine(session, false);
        let _ = self.commands.send(Request::Create {
            request: fresh(),
            session,
            goal: goal.to_owned(),
            checkout: self.checkout.clone(),
        });

        Ok(engine)
    }
}

impl Drop for BusRoster {
    fn drop(&mut self) {
        let _ = self.commands.send(Request::Stop);
    }
}

/// One session, read from the daemon's record stream.
pub struct RemoteEngine {
    session: SessionID,
    commands: UnboundedSender<Request>,
    updates: Receiver<Internal>,
    fold: Transcript,
    seed: Option<TaskRecord>,
    history: Option<History>,
    queued: Vec<SessionCommit>,
    pending: bool,
    ready: bool,
    refusal: Option<mokmok_session_wire::Refusal>,
    link: Link,
}

struct History {
    request: CommandID,
    next: u32,
    records: Vec<Record>,
    seed: Option<TaskRecord>,
}

impl RemoteEngine {
    const fn new(
        session: SessionID,
        commands: UnboundedSender<Request>,
        updates: Receiver<Internal>,
    ) -> Self {
        Self {
            session,
            commands,
            updates,
            fold: Transcript::new(session),
            seed: None,
            history: None,
            queued: Vec::new(),
            pending: true,
            ready: false,
            refusal: None,
            link: Link::Connected,
        }
    }

    /// Waits for the first fold, before the screen exists.
    fn ready(
        mut self,
        within: Duration,
    ) -> Result<Self, Error> {
        let deadline = Instant::now() + within;

        while !self.ready {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = self
                .updates
                .recv_timeout(left)
                .map_err(|_| Error::BusThread)?;
            self.ingest(event);

            if let Some(reason) = self.refusal.take() {
                return Err(Error::Refused(reason));
            }
        }

        Ok(self)
    }

    /// Folds one routed event into the cache and reports what the screen should hear.
    fn ingest(
        &mut self,
        event: Internal,
    ) -> Vec<Update> {
        let mut updates = Vec::new();

        match event {
            Internal::Commit(commit) => {
                if self.history.is_some() {
                    self.queued.push(commit);

                    return updates;
                }
                self.commit(&commit);
                updates.push(Update::Stepped(Ok(Progress::Awaiting)));
            },
            Internal::History(chunk) => {
                if self.history(chunk) {
                    updates.push(Update::Stepped(Ok(Progress::Awaiting)));
                }
            },
            Internal::Refused(reason) => {
                self.pending = false;
                self.refusal = Some(reason.clone());
                updates.push(Update::Refused(Error::Refused(reason)));
            },
            Internal::Seeded(task) => {
                self.seed = Some(task.clone());
                if self.history.is_none() {
                    let _ = self.fold.apply(&Record::Task(task));
                }
            },
            Internal::Trouble(detail) => {
                self.pending = false;
                updates.push(Update::Trouble(detail));
            },
            Internal::Progress(text) => updates.push(Update::Fragment(text)),
            Internal::Link(link) => {
                self.link = link;
                updates.push(Update::Link(link));
            },
        }

        updates
    }

    fn commit(
        &mut self,
        commit: &SessionCommit,
    ) {
        for record in &commit.records {
            if let Err(_refusal) = self.fold.apply(record) {
                self.pending = false;
                self.refusal = None;

                return;
            }
        }
        self.pending = false;
        self.ready = true;
    }

    /// Folds one history chunk. Answers whether this chunk completed a replacement.
    fn history(
        &mut self,
        chunk: HistoryChunk,
    ) -> bool {
        let restart = self
            .history
            .as_ref()
            .is_none_or(|history| history.request != chunk.request || history.next != chunk.index);
        if restart {
            self.history = Some(History {
                request: chunk.request,
                next: 0,
                records: Vec::new(),
                seed: None,
            });
        }
        let Some(history) = self.history.as_mut() else {
            return false;
        };
        history.records.extend(chunk.records);
        if chunk.seed.is_some() {
            history.seed.clone_from(&chunk.seed);
        }
        history.next = chunk.index.saturating_add(1);

        // Only the last chunk completes the replacement. The records of every earlier chunk have
        // already arrived in order, so a duplicate chunk folds as `Ack::Duplicate`.
        if !chunk.last {
            return false;
        }

        let Some(history) = self.history.take() else {
            return false;
        };
        self.fold = match Transcript::replay(self.session, &history.records) {
            Ok(fold) => fold,
            Err(_refusal) => {
                self.pending = false;

                return false;
            },
        };
        if self.fold.task.is_none()
            && let Some(seed) = history.seed.or_else(|| self.seed.clone())
        {
            self.seed = Some(seed.clone());
            let _ = self.fold.apply(&Record::Task(seed));
        }
        let queued = std::mem::take(&mut self.queued);
        for commit in queued {
            self.commit(&commit);
        }
        self.pending = false;
        self.ready = true;

        true
    }
}

impl Engine for RemoteEngine {
    fn session(&self) -> SessionID {
        self.session
    }

    fn fold(&self) -> Result<Transcript, Error> {
        Ok(self.fold.clone())
    }

    fn speak(
        &mut self,
        line: &str,
    ) -> Result<(), Error> {
        self.commands
            .send(Request::Speak {
                request: fresh(),
                session: self.session,
                line: line.to_owned(),
            })
            .map_err(|_| Error::BusThread)?;
        self.pending = true;

        Ok(())
    }

    fn step(&mut self) -> impl std::future::Future<Output = Result<Progress, Error>> {
        std::future::ready(Ok(Progress::Awaiting))
    }

    fn reopen(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn busy(&self) -> bool {
        // An empty fold is a refusal or a history still on its way, and `pending` already covers
        // the wait. The core's own rule would call an unopened fold with no task "working", which
        // is right for a local goal run and wrong for a remote session the daemon refused.
        self.pending
            || matches!(self.fold.session.phase(), Phase::Draining)
            || (self.fold.task.is_some() && working(&self.fold))
    }

    fn drain(&mut self) -> Vec<Update> {
        let mut updates = Vec::new();

        loop {
            match self.updates.try_recv() {
                Ok(event) => updates.extend(self.ingest(event)),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return updates,
            }
        }
    }
}

fn bus_thread(
    socket: &Path,
    nonce: Uuid,
    shared: &Arc<Shared>,
    requests: UnboundedReceiver<Request>,
    ready: &Sender<Result<Attached, Error>>,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        let _ = ready.send(Err(Error::BusThread));

        return;
    };

    match runtime.block_on(attach(socket, nonce, shared, requests, ready)) {
        Ok(()) => {},
        Err(_error) => {
            // A handshake failure is sent on `ready`; a failure after it is the bus going away, and
            // the engines already carry a link update. Nothing else owns the process here.
        },
    }
}

fn fresh() -> CommandID {
    CommandID::from_uuid(Uuid::new_v4())
}

async fn attach(
    socket: &Path,
    nonce: Uuid,
    shared: &Arc<Shared>,
    mut requests: UnboundedReceiver<Request>,
    ready: &Sender<Result<Attached, Error>>,
) -> Result<(), Error> {
    let client = Client::connect(socket).await?;
    let mut global = client
        .subscribe(command_topic(), SubscriberId::new())
        .await?;
    let hello = fresh();
    publish(
        &client,
        command_topic(),
        &Command::Hello {
            request: hello,
            protocol: mokmok_session_wire::PROTOCOL,
        },
    )
    .await?;
    let daemon = match welcome(&mut global, hello).await {
        Ok(daemon) => daemon,
        Err(error) => {
            let _ = ready.send(Err(error));

            return Ok(());
        },
    };
    let mut daemon_events = client
        .subscribe(daemon_topic(daemon), daemon_view_subscriber(nonce, daemon))
        .await?;
    let list = fresh();
    publish(&client, command_topic(), &Command::List { request: list }).await?;
    let roster = match roster(&mut daemon_events).await {
        Ok(roster) => roster,
        Err(error) => {
            let _ = ready.send(Err(error));

            return Ok(());
        },
    };
    {
        let mut cached = shared.roster.lock().map_err(|_| Error::BusThread)?;
        cached.clone_from(&roster);
    }
    let _ = ready.send(Ok(Attached { roster }));

    run(&client, nonce, shared, global, daemon_events, &mut requests).await
}

async fn publish<T: serde::Serialize + ?Sized>(
    client: &Client,
    topic: Topic,
    message: &T,
) -> Result<(), Error> {
    let payload = Payload::json(message)?;

    client
        .publish(topic, payload)
        .await
        .map(|_| ())
        .map_err(Error::Bus)
}

async fn welcome(
    subscription: &mut Subscription,
    request: CommandID,
) -> Result<AgentID, Error> {
    loop {
        let batch = next(subscription).await?;
        for event in batch.events() {
            let Ok(reply) = event.payload().decode::<Reply>() else {
                continue;
            };
            match reply {
                Reply::Welcome {
                    request: answered,
                    daemon,
                    protocol,
                } if answered == request => {
                    if protocol != mokmok_session_wire::PROTOCOL {
                        return Err(Error::Refused(mokmok_session_wire::Refusal::Unsupported {
                            protocol,
                        }));
                    }

                    let _ = subscription.ack(batch.after()).await;

                    return Ok(daemon);
                },
                Reply::Ack {
                    request: answered,
                    outcome: Outcome::Refused { reason },
                } if answered == request => {
                    return Err(Error::Refused(reason));
                },
                Reply::Welcome { .. } | Reply::Ack { .. } => {},
            }
        }
        subscription.ack(batch.after()).await?;
    }
}

async fn roster(subscription: &mut Subscription) -> Result<Vec<RosterEntry>, Error> {
    let mut revision = 0u64;
    let mut entries = Vec::new();

    loop {
        let batch = next(subscription).await?;
        for event in batch.events() {
            let Ok(DaemonEvent::Roster(chunk)) = event.payload().decode::<DaemonEvent>() else {
                continue;
            };
            if chunk.revision != revision {
                revision = chunk.revision;
                entries.clear();
            }
            entries.extend(chunk.entries);
            if chunk.last {
                let _ = subscription.ack(batch.after()).await;

                return Ok(entries);
            }
        }
        subscription.ack(batch.after()).await?;
    }
}

async fn next(subscription: &mut Subscription) -> Result<mokmok_event_bus::Batch, Error> {
    tokio::time::timeout(ATTACH, subscription.next())
        .await
        .map_err(|_| Error::BusThread)?
        .map_err(Error::Bus)
}

/// One event a subscription task hands the bus thread.
enum BusEvent {
    Reply(Reply),
    Daemon(DaemonEvent),
}

#[allow(clippy::too_many_lines)]
async fn run(
    client: &Client,
    nonce: Uuid,
    shared: &Arc<Shared>,
    global: Subscription,
    daemon_events: Subscription,
    requests: &mut UnboundedReceiver<Request>,
) -> Result<(), Error> {
    let mut watchers: HashMap<SessionID, Sender<Internal>> = HashMap::new();
    let mut pending: HashMap<CommandID, Sender<Internal>> = HashMap::new();
    let mut roster_revision = 0u64;
    let mut roster_entries: Vec<RosterEntry> = Vec::new();
    // One task per subscription, each owning its `Subscription`. A `next()` future cannot be
    // cancelled and asked again: the server keeps the parked pull, and the second pull is refused
    // `pull_in_flight`. A task that always awaits to completion is the shape that respects that.
    let (events, mut received) = unbounded_channel();
    let out = events.clone();
    tokio::spawn(async move {
        read_global(global, out).await;
    });
    let out = events.clone();
    tokio::spawn(async move {
        read_daemon(daemon_events, out).await;
    });

    loop {
        tokio::select! {
            Some(command) = requests.recv() => match command {
                Request::Watch { session, updates, open } => {
                    if watchers.contains_key(&session) {
                        continue;
                    }
                    let subscription = match client
                        .subscribe(
                            session_topic(session),
                            session_subscriber(nonce, session),
                        )
                        .await
                    {
                        Ok(subscription) => subscription,
                        Err(error) => {
                            let _ = updates.send(Internal::Link(Link::Reconnecting));
                            return Err(Error::Bus(error));
                        },
                    };
                    let events = updates.clone();
                    tokio::spawn(async move { read_session(subscription, events).await; });
                    watchers.insert(session, updates.clone());
                    if open {
                        let request = fresh();
                        pending.insert(request, updates.clone());
                        publish(client, command_topic(), &Command::Open { request, session }).await?;
                        // A created session has no log yet, so its history is asked for after the
                        // `Create` seeds it. A history asked for before that is refused.
                        let history = fresh();
                        publish(
                            client,
                            command_topic(),
                            &Command::History {
                                request: history,
                                session,
                            },
                        )
                        .await?;
                    }
                },
                Request::Create { request, session, goal, checkout } => {
                    if let Some(updates) = watchers.get(&session) {
                        pending.insert(request, updates.clone());
                    }
                    publish(
                        client,
                        command_topic(),
                        &Command::Create {
                            request,
                            session,
                            goal,
                            checkout,
                        },
                    )
                    .await?;
                    let history = fresh();
                    publish(
                        client,
                        command_topic(),
                        &Command::History {
                            request: history,
                            session,
                        },
                    )
                    .await?;
                },
                Request::Speak { request, session, line } => {
                    if let Some(updates) = watchers.get(&session) {
                        pending.insert(request, updates.clone());
                    }
                    publish(
                        client,
                        command_topic(),
                        &Command::Speak {
                            request,
                            session,
                            line,
                        },
                    )
                    .await?;
                },
                Request::List => {
                    publish(
                        client,
                        command_topic(),
                        &Command::List { request: fresh() },
                    )
                    .await?;
                },
                Request::Stop => return Ok(()),
            },
            Some(event) = received.recv() => match event {
                BusEvent::Reply(reply) => {
                    if let Reply::Ack {
                        request,
                        outcome: Outcome::Refused { reason },
                    } = reply
                        && let Some(updates) = pending.remove(&request)
                    {
                        let _ = updates.send(Internal::Refused(reason));
                    }
                },
                BusEvent::Daemon(event) => match event {
                    DaemonEvent::Progress { session, text } => {
                        if let Some(updates) = watchers.get(&session) {
                            let _ = updates.send(Internal::Progress(text));
                        }
                    },
                    DaemonEvent::Roster(chunk) => {
                        if chunk.revision != roster_revision {
                            roster_revision = chunk.revision;
                            roster_entries.clear();
                        }
                        roster_entries.extend(chunk.entries);
                        if chunk.last {
                            let mut cached =
                                shared.roster.lock().map_err(|_| Error::BusThread)?;
                            cached.clone_from(&roster_entries);
                        }
                    },
                },
            },
        }
    }
}

async fn read_global(
    mut subscription: Subscription,
    events: UnboundedSender<BusEvent>,
) {
    loop {
        let Ok(batch) = subscription.next().await else {
            return;
        };
        for event in batch.events() {
            if let Ok(reply) = event.payload().decode::<Reply>() {
                let _ = events.send(BusEvent::Reply(reply));
            }
        }
        if subscription.ack(batch.after()).await.is_err() {
            return;
        }
    }
}

async fn read_daemon(
    mut subscription: Subscription,
    events: UnboundedSender<BusEvent>,
) {
    loop {
        let Ok(batch) = subscription.next().await else {
            return;
        };
        for event in batch.events() {
            if let Ok(event) = event.payload().decode::<DaemonEvent>() {
                let _ = events.send(BusEvent::Daemon(event));
            }
        }
        if subscription.ack(batch.after()).await.is_err() {
            return;
        }
    }
}

async fn read_session(
    mut subscription: Subscription,
    updates: Sender<Internal>,
) {
    loop {
        let batch = match subscription.next().await {
            Ok(batch) => batch,
            Err(error) => {
                let _ = updates.send(Internal::Link(Link::Reconnecting));
                let _ = updates.send(Internal::Trouble(format!("the bus stopped: {error}")));

                return;
            },
        };

        for event in batch.events() {
            let internal = match event.payload().decode::<SessionEvent>() {
                Ok(SessionEvent::Commit(commit)) => Internal::Commit(commit),
                Ok(SessionEvent::History(chunk)) => Internal::History(chunk),
                Ok(SessionEvent::HistoryRefused { reason, .. }) => Internal::Refused(reason),
                Ok(SessionEvent::Seeded { task, .. }) => Internal::Seeded(task),
                Ok(SessionEvent::Trouble { detail, .. }) => Internal::Trouble(detail),
                Err(_) => continue,
            };
            if updates.send(internal).is_err() {
                return;
            }
        }
        if subscription.ack(batch.after()).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc::{Sender, channel};
    use std::time::Duration;

    use mokmok_agent_runtime::{
        Budget, Record, Setting, SpeechRecord, Task, TaskRecord, Transcript, turn_id,
    };
    use mokmok_protocol::{CommandID, SessionID};
    use mokmok_session_wire::{HistoryChunk, Notice, Refusal, SessionCommit};
    use mokmok_state_machine::{Fact, Phase, TurnFact, TurnNumber};
    use tokio::sync::mpsc::unbounded_channel;
    use uuid::Uuid;

    use super::{Internal, RemoteEngine, Request};
    use crate::port::{Engine, Link, Update};

    fn session() -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(1))
    }

    fn fresh() -> CommandID {
        CommandID::from_uuid(Uuid::from_u128(2))
    }

    fn engine() -> (RemoteEngine, Sender<Internal>) {
        let (updates, received) = channel();
        let (commands, _requests) = unbounded_channel::<Request>();

        (RemoteEngine::new(session(), commands, received), updates)
    }

    fn task() -> Task {
        Task {
            goal: "goal".to_owned(),
            setting: Setting {
                checkout: PathBuf::from("/tmp/checkout"),
                readable: Vec::new(),
                writable: Vec::new(),
                program: PathBuf::from("/bin/sh"),
                deadline: Duration::from_secs(1),
                search: Vec::new(),
                skills: Vec::new(),
                delegate: false,
            },
            budget: Budget {
                turns: 1,
                attempts: 1,
                tokens: 0,
                observation_bytes: 0,
                context_turns: 0,
                context_bytes: 0,
            },
        }
    }

    fn opened() -> Vec<Record> {
        vec![
            Record::Session {
                fact: Fact::SessionOpened,
            },
            Record::Task(TaskRecord { task: task() }),
            Record::Turn {
                turn: turn_id(session(), 1),
                fact: TurnFact::TurnOpened {
                    number: TurnNumber::new(1),
                },
            },
        ]
    }

    fn commit(records: Vec<Record>) -> SessionCommit {
        SessionCommit {
            session: session(),
            command: fresh(),
            notice: Notice {
                turn: None,
                execution: None,
                resolution: None,
                at: mokmok_protocol::Moment::EPOCH,
            },
            records,
        }
    }

    fn speech() -> Record {
        Record::Speech(SpeechRecord {
            turn: turn_id(session(), 1),
            text: "hello".to_owned(),
        })
    }

    #[test]
    fn a_commit_folds_and_the_engine_stops_reporting_a_pending_line() {
        let (mut engine, updates) = engine();

        updates.send(Internal::Commit(commit(opened()))).unwrap();
        let drained = engine.drain();

        assert!(
            drained
                .iter()
                .any(|update| matches!(update, Update::Stepped(Ok(_)))),
        );
        assert!(engine.busy(), "the daemon still owes the turn");
        assert_eq!(engine.fold().unwrap().session.phase(), Phase::Accepting);
    }

    #[test]
    fn the_same_commit_twice_draws_one_fold() {
        let (mut engine, updates) = engine();
        let mut records = opened();
        records.push(Record::Speech(speech_record()));
        let commit = commit(records);

        updates.send(Internal::Commit(commit.clone())).unwrap();
        engine.drain();
        updates.send(Internal::Commit(commit)).unwrap();
        engine.drain();

        assert_eq!(engine.fold().unwrap().turns.len(), 1);
        assert_eq!(
            engine.fold().unwrap().speech(turn_id(session(), 1)),
            Some("hello")
        );
    }

    fn speech_record() -> SpeechRecord {
        SpeechRecord {
            turn: turn_id(session(), 1),
            text: "hello".to_owned(),
        }
    }

    #[test]
    fn a_refusal_is_a_screen_update_and_ends_the_pending_line() {
        let (mut engine, updates) = engine();

        updates
            .send(Internal::Refused(Refusal::NotAwaiting {
                session: session(),
            }))
            .unwrap();
        let drained = engine.drain();

        assert!(
            drained
                .iter()
                .any(|update| matches!(update, Update::Refused(_))),
        );
        assert!(!engine.busy());
    }

    #[test]
    fn a_link_update_reaches_the_screen() {
        let (mut engine, updates) = engine();

        updates.send(Internal::Link(Link::Reconnecting)).unwrap();
        let drained = engine.drain();

        assert!(
            drained
                .iter()
                .any(|update| matches!(update, Update::Link(Link::Reconnecting))),
        );
    }

    #[test]
    fn a_history_replacement_folds_the_whole_log() {
        let (mut engine, updates) = engine();

        updates
            .send(Internal::History(HistoryChunk {
                request: fresh(),
                session: session(),
                index: 0,
                last: true,
                seed: None,
                records: opened(),
            }))
            .unwrap();
        let drained = engine.drain();

        assert!(
            drained
                .iter()
                .any(|update| matches!(update, Update::Stepped(Ok(_)))),
        );
        assert_eq!(engine.fold().unwrap().session.phase(), Phase::Accepting);
    }

    #[test]
    fn a_commit_that_arrives_during_the_history_is_applied_after_it() {
        let (mut engine, updates) = engine();
        let request = fresh();

        updates
            .send(Internal::History(HistoryChunk {
                request,
                session: session(),
                index: 0,
                last: false,
                seed: None,
                records: opened(),
            }))
            .unwrap();
        updates
            .send(Internal::Commit(commit(vec![speech()])))
            .unwrap();
        updates
            .send(Internal::History(HistoryChunk {
                request,
                session: session(),
                index: 1,
                last: true,
                seed: None,
                records: Vec::new(),
            }))
            .unwrap();
        engine.drain();

        let fold = engine.fold().unwrap();
        assert_eq!(fold.session.phase(), Phase::Accepting);
        assert_eq!(fold.speech(turn_id(session(), 1)), Some("hello"));
    }

    #[test]
    fn a_seeded_task_is_visible_before_the_log_holds_it() {
        let (mut engine, updates) = engine();

        updates
            .send(Internal::Seeded(TaskRecord { task: task() }))
            .unwrap();
        engine.drain();

        assert_eq!(engine.fold().unwrap().task.unwrap().goal, "goal");
    }

    #[test]
    fn a_fold_that_starts_empty_is_ready_after_the_first_history() {
        let (mut engine, updates) = engine();
        assert!(engine.busy(), "the history is still on its way");

        updates
            .send(Internal::History(HistoryChunk {
                request: fresh(),
                session: session(),
                index: 0,
                last: true,
                seed: Some(TaskRecord { task: task() }),
                records: Vec::new(),
            }))
            .unwrap();
        engine.drain();

        assert_eq!(engine.fold().unwrap().task.unwrap().goal, "goal");
        assert!(!engine.busy(), "a seeded session waits for its first line");
        let _ = Transcript::new(session());
    }
}
