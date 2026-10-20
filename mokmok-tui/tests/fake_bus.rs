//! The terminal's bus adapter against a stand-in daemon on a real bus, and the built binary on a
//! pipe and a pseudo-terminal.
//!
//! The daemon is stood in for so the cases run wherever this package is built, without another
//! package's binary to start and stop.

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write as _;
    use std::num::NonZeroUsize;
    use std::os::fd::AsFd;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use mokmok_agent_runtime::{
        Budget, Record, Setting, SpeechRecord, Task, TaskRecord, Transcript, turn_id,
    };
    use mokmok_event_bus::{Client, Config, Payload, SubscriberId, Topic, serve};
    use mokmok_protocol::{AgentID, CommandID, Moment, SessionID};
    use mokmok_session_wire::{
        Command as WireCommand, DaemonEvent, HistoryChunk, Notice, Outcome, PROTOCOL, Refusal,
        Reply, RosterChunk, RosterEntry, SessionCommit, SessionEvent, command_topic, daemon_topic,
        session_topic,
    };
    use mokmok_state_machine::{Attempt, Ending, Fact, Phase, TurnFact, TurnNumber};
    use mokmok_tui::{BusRoster, Engine, Error, Listing, Roster, Update};
    use nix::errno::Errno;
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use nix::pty::{Winsize, openpty};
    use nix::sys::termios::{LocalFlags, tcgetattr};
    use nix::unistd::read;
    use uuid::Uuid;

    const PATIENCE: Duration = Duration::from_secs(1);
    const POLL: Duration = Duration::from_millis(5);

    const CTRL_C: &[u8] = b"\x03";
    const LEAVE_ALTERNATE: &[u8] = b"\x1b[?1049l";
    const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mokmok-tui-fake-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join("checkout")).expect("the scratch checkout is made");

            Self(path)
        }

        fn socket(&self) -> PathBuf {
            self.0.join("bus.sock")
        }

        fn store(&self) -> PathBuf {
            self.0.join("bus.db")
        }

        fn checkout(&self) -> PathBuf {
            std::fs::canonicalize(self.0.join("checkout")).expect("the checkout is real")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn daemon() -> AgentID {
        AgentID::from_uuid(Uuid::from_u128(7))
    }

    fn another_daemon() -> AgentID {
        AgentID::from_uuid(Uuid::from_u128(8))
    }

    fn session() -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(1))
    }

    fn another_session() -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(2))
    }

    fn request(seed: u128) -> CommandID {
        CommandID::from_uuid(Uuid::from_u128(seed))
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
                skills: Vec::new(),
                delegate: false,
            },
            budget: Budget::default(),
        }
    }

    fn task_record() -> TaskRecord {
        TaskRecord { task: task() }
    }

    /// A session that has begun and holds one turn the human has not spoken into.
    fn opened_records() -> Vec<Record> {
        let turn = turn_id(session(), 1);

        vec![
            Record::Session {
                fact: Fact::SessionOpened,
            },
            Record::Task(task_record()),
            Record::Turn {
                turn,
                fact: TurnFact::TurnOpened {
                    number: TurnNumber::new(1),
                },
            },
        ]
    }

    /// The same session with a line in flight, so its turn is one the daemon still owes.
    fn working_records() -> Vec<Record> {
        let turn = turn_id(session(), 1);
        let mut records = opened_records();

        records.push(Record::Speech(SpeechRecord {
            turn,
            text: "before".to_owned(),
        }));
        records.push(Record::Turn {
            turn,
            fact: TurnFact::ModelRequested {
                attempt: Attempt::new(1),
            },
        });

        records
    }

    fn entry(
        session: SessionID,
        goal: &str,
    ) -> RosterEntry {
        RosterEntry {
            session,
            phase: Phase::Accepting,
            goal: Some(goal.to_owned()),
            checkout: Some(PathBuf::from("/checkout")),
            turns: 0,
        }
    }

    #[derive(Clone)]
    enum HistoryReply {
        Folded(Vec<Record>),
        Chunks(Vec<HistorySpec>),
        Refused(Refusal),
        Delayed(Duration, Vec<Record>),
    }

    #[derive(Clone)]
    struct HistorySpec {
        index: u32,
        last: bool,
        seed: Option<TaskRecord>,
        records: Vec<Record>,
    }

    #[derive(Clone)]
    enum SpeakReply {
        Echo,
        Refused(Refusal),
    }

    #[derive(Clone)]
    enum CancelReply {
        Closing,
        Refused(Refusal),
    }

    #[derive(Clone)]
    struct Behaviour {
        protocol: u16,
        welcome_distractor: bool,
        refusal_distractor: bool,
        hello_refusal: Option<Refusal>,
        roster_chunks: Vec<Vec<RosterEntry>>,
        history: HistoryReply,
        speak: SpeakReply,
        cancel: CancelReply,
    }

    impl Default for Behaviour {
        fn default() -> Self {
            Self {
                protocol: PROTOCOL,
                welcome_distractor: false,
                refusal_distractor: false,
                hello_refusal: None,
                roster_chunks: vec![vec![entry(session(), "hold a conversation")]],
                history: HistoryReply::Folded(opened_records()),
                speak: SpeakReply::Echo,
                cancel: CancelReply::Closing,
            }
        }
    }

    /// A bus and the daemon that answers on it, on this case's own socket and database.
    struct Fake {
        socket: PathBuf,
        behaviour: Arc<Mutex<Behaviour>>,
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Fake {
        fn start(
            scratch: &Scratch,
            behaviour: Behaviour,
        ) -> Self {
            Self::with_connections(scratch, behaviour, 512)
        }

        fn with_connections(
            scratch: &Scratch,
            behaviour: Behaviour,
            connections: usize,
        ) -> Self {
            let socket = scratch.socket();
            let mut config = Config {
                path: socket.clone(),
                store: scratch.store(),
                ..Config::default()
            };
            config.limits.connections =
                NonZeroUsize::new(connections).expect("the case asks for a connection");
            let behaviour = Arc::new(Mutex::new(behaviour));
            let revision = Arc::new(AtomicU64::new(0));
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let served_socket = socket.clone();
            let daemon_behaviour = Arc::clone(&behaviour);
            let daemon_revision = Arc::clone(&revision);
            let thread = std::thread::Builder::new()
                .name("fake-daemon".to_owned())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("the fake daemon's runtime starts");
                    runtime.block_on(async move {
                        // The server owns this runtime's only blocking await. A daemon that answered
                        // inside the same `select!` could win the race and leave a parked connection
                        // open, which is exactly the state the reconnect case is about.
                        tokio::spawn(respond(served_socket, daemon_behaviour, daemon_revision));
                        let _ = serve(config, async move {
                            let _ = stopped.await;
                        })
                        .await;
                    });
                })
                .expect("the fake daemon starts");

            wait_for_socket(&socket);

            Self {
                socket,
                behaviour,
                stop: Some(stop),
                thread: Some(thread),
            }
        }

        fn set_roster(
            &self,
            entries: Vec<RosterEntry>,
        ) {
            let mut behaviour = self.behaviour.lock().expect("the behaviour is readable");

            behaviour.roster_chunks = vec![entries];
        }

        fn stop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn wait_for_socket(path: &Path) {
        let deadline = Instant::now() + PATIENCE;

        while Instant::now() < deadline {
            if path.exists() {
                return;
            }
            std::thread::sleep(POLL);
        }

        panic!("the fake daemon never bound {}", path.display());
    }

    async fn connect(socket: &Path) -> Option<Client> {
        let deadline = Instant::now() + PATIENCE;

        loop {
            if let Ok(client) = Client::connect(socket).await {
                return Some(client);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    async fn send<T: serde::Serialize>(
        client: &Client,
        topic: Topic,
        message: &T,
    ) {
        let Ok(payload) = Payload::json(message) else {
            return;
        };
        let _ = client.publish(topic, payload).await;
    }

    async fn respond(
        socket: PathBuf,
        behaviour: Arc<Mutex<Behaviour>>,
        revision: Arc<AtomicU64>,
    ) {
        let Some(client) = connect(&socket).await else {
            return;
        };
        let Ok(mut commands) = client.subscribe(command_topic(), SubscriberId::new()).await else {
            return;
        };

        loop {
            let Ok(batch) = commands.next().await else {
                return;
            };
            for event in batch.events() {
                let Ok(command) = event.payload().decode::<WireCommand>() else {
                    continue;
                };
                let behaviour = behaviour.lock().expect("the behaviour is readable").clone();
                handle(&client, &behaviour, &revision, command).await;
            }
            if commands.ack(batch.after()).await.is_err() {
                return;
            }
        }
    }

    async fn handle(
        client: &Client,
        behaviour: &Behaviour,
        revision: &AtomicU64,
        command: WireCommand,
    ) {
        match command {
            WireCommand::Hello { request: hello, .. } => {
                if behaviour.refusal_distractor {
                    send(
                        client,
                        command_topic(),
                        &Reply::Ack {
                            request: request(998),
                            outcome: Outcome::Refused {
                                reason: Refusal::Malformed {
                                    detail: "a command this client did not send".to_owned(),
                                },
                            },
                        },
                    )
                    .await;
                }
                if behaviour.welcome_distractor {
                    send(
                        client,
                        command_topic(),
                        &Reply::Welcome {
                            request: request(999),
                            daemon: another_daemon(),
                            protocol: behaviour.protocol,
                        },
                    )
                    .await;
                }
                match &behaviour.hello_refusal {
                    Some(reason) => {
                        send(
                            client,
                            command_topic(),
                            &Reply::Ack {
                                request: hello,
                                outcome: Outcome::Refused {
                                    reason: reason.clone(),
                                },
                            },
                        )
                        .await;
                    },
                    None => {
                        send(
                            client,
                            command_topic(),
                            &Reply::Welcome {
                                request: hello,
                                daemon: daemon(),
                                protocol: behaviour.protocol,
                            },
                        )
                        .await;
                    },
                }
            },
            WireCommand::List { .. } => {
                let revision = revision.fetch_add(1, Ordering::Relaxed);
                roster(client, behaviour, revision).await;
            },
            WireCommand::Open { request, session }
            | WireCommand::Create {
                request, session, ..
            } => {
                accepted(client, request, session).await;
            },
            WireCommand::History { request, session } => {
                history(client, behaviour, request, session).await;
            },
            WireCommand::Speak {
                request,
                session,
                line,
            } => {
                speak(client, behaviour, request, session, &line).await;
            },
            WireCommand::Cancel { request, session } => {
                cancel(client, behaviour, request, session).await;
            },
        }
    }

    async fn accepted(
        client: &Client,
        request: CommandID,
        session: SessionID,
    ) {
        send(
            client,
            command_topic(),
            &Reply::Ack {
                request,
                outcome: Outcome::Accepted { session },
            },
        )
        .await;
    }

    async fn roster(
        client: &Client,
        behaviour: &Behaviour,
        revision: u64,
    ) {
        let count = behaviour.roster_chunks.len();
        for (index, entries) in behaviour.roster_chunks.iter().enumerate() {
            let chunk = RosterChunk {
                daemon: daemon(),
                revision,
                index: u32::try_from(index).expect("a roster chunk index fits"),
                last: index + 1 == count,
                entries: entries.clone(),
            };
            send(client, daemon_topic(daemon()), &DaemonEvent::Roster(chunk)).await;
        }
    }

    async fn history(
        client: &Client,
        behaviour: &Behaviour,
        request: CommandID,
        session: SessionID,
    ) {
        match &behaviour.history {
            HistoryReply::Folded(records) => {
                chunk(client, request, session, 0, true, None, records.clone()).await;
            },
            HistoryReply::Chunks(specs) => {
                for spec in specs {
                    chunk(
                        client,
                        request,
                        session,
                        spec.index,
                        spec.last,
                        spec.seed.clone(),
                        spec.records.clone(),
                    )
                    .await;
                }
            },
            HistoryReply::Refused(reason) => {
                send(
                    client,
                    session_topic(session),
                    &SessionEvent::HistoryRefused {
                        request,
                        session,
                        reason: reason.clone(),
                    },
                )
                .await;
            },
            HistoryReply::Delayed(delay, records) => {
                tokio::time::sleep(*delay).await;
                chunk(client, request, session, 0, true, None, records.clone()).await;
            },
        }
    }

    async fn chunk(
        client: &Client,
        request: CommandID,
        session: SessionID,
        index: u32,
        last: bool,
        seed: Option<TaskRecord>,
        records: Vec<Record>,
    ) {
        send(
            client,
            session_topic(session),
            &SessionEvent::History(HistoryChunk {
                request,
                session,
                index,
                last,
                seed,
                records,
            }),
        )
        .await;
    }

    async fn speak(
        client: &Client,
        behaviour: &Behaviour,
        request: CommandID,
        session: SessionID,
        line: &str,
    ) {
        match &behaviour.speak {
            SpeakReply::Echo => {
                accepted(client, request, session).await;
                let turn = turn_id(session, 1);
                let commit = SessionCommit {
                    session,
                    command: request,
                    notice: Notice {
                        turn: Some(turn),
                        execution: None,
                        resolution: None,
                        at: Moment::EPOCH,
                    },
                    records: vec![Record::Speech(SpeechRecord {
                        turn,
                        text: line.to_owned(),
                    })],
                };
                send(
                    client,
                    session_topic(session),
                    &SessionEvent::Commit(commit),
                )
                .await;
            },
            SpeakReply::Refused(reason) => {
                send(
                    client,
                    command_topic(),
                    &Reply::Ack {
                        request,
                        outcome: Outcome::Refused {
                            reason: reason.clone(),
                        },
                    },
                )
                .await;
            },
        }
    }

    async fn cancel(
        client: &Client,
        behaviour: &Behaviour,
        request: CommandID,
        session: SessionID,
    ) {
        match &behaviour.cancel {
            CancelReply::Closing => {
                accepted(client, request, session).await;
                let commit = SessionCommit {
                    session,
                    command: request,
                    notice: Notice {
                        turn: None,
                        execution: None,
                        resolution: None,
                        at: Moment::EPOCH,
                    },
                    records: vec![
                        Record::Session {
                            fact: Fact::CancelRequested,
                        },
                        Record::Session {
                            fact: Fact::SessionClosed,
                        },
                    ],
                };
                send(
                    client,
                    session_topic(session),
                    &SessionEvent::Commit(commit),
                )
                .await;
            },
            CancelReply::Refused(reason) => {
                send(
                    client,
                    command_topic(),
                    &Reply::Ack {
                        request,
                        outcome: Outcome::Refused {
                            reason: reason.clone(),
                        },
                    },
                )
                .await;
            },
        }
    }

    fn attached(
        scratch: &Scratch,
        behaviour: Behaviour,
    ) -> (Fake, Result<BusRoster, Error>) {
        let fake = Fake::start(scratch, behaviour);
        let roster = BusRoster::attach(&fake.socket, Moment::EPOCH, scratch.checkout());

        (fake, roster)
    }

    fn wait_for_listing(
        roster: &BusRoster,
        done: impl Fn(&[Listing]) -> bool,
    ) -> Vec<Listing> {
        let deadline = Instant::now() + PATIENCE;

        loop {
            let listed = roster.listed().expect("the roster reads");
            if done(&listed) {
                return listed;
            }
            assert!(
                Instant::now() < deadline,
                "the listing never changed: {listed:?}"
            );
            std::thread::sleep(POLL);
        }
    }

    fn wait_for_fold(
        engine: &mut impl Engine,
        done: impl Fn(&Transcript) -> bool,
    ) -> Transcript {
        let deadline = Instant::now() + PATIENCE;

        loop {
            let _ = engine.drain();
            let fold = engine.fold().expect("the fold reads");
            if done(&fold) {
                return fold;
            }
            assert!(
                Instant::now() < deadline,
                "the fold never reached the state"
            );
            std::thread::sleep(POLL);
        }
    }

    fn wait_for_update(
        engine: &mut impl Engine,
        done: impl Fn(&Update) -> bool,
    ) -> Update {
        let deadline = Instant::now() + PATIENCE;

        loop {
            for update in engine.drain() {
                if done(&update) {
                    return update;
                }
            }
            assert!(Instant::now() < deadline, "the update never arrived");
            std::thread::sleep(POLL);
        }
    }

    #[test]
    fn attach_without_a_bus_fails() {
        let scratch = Scratch::new("absent");
        let error = BusRoster::attach(&scratch.socket(), Moment::EPOCH, scratch.checkout())
            .err()
            .expect("a missing socket is a refusal");

        assert!(matches!(error, Error::Bus(_)), "{error:?}");
        assert!(!scratch.socket().exists(), "the client made no socket");
    }

    #[test]
    fn attach_ignores_a_welcome_addressed_to_another_request() {
        let scratch = Scratch::new("welcome-distractor");
        let (_fake, roster) = attached(
            &scratch,
            Behaviour {
                welcome_distractor: true,
                ..Behaviour::default()
            },
        );
        let roster = roster.expect("the matching welcome is the one that answers");

        assert_eq!(roster.listed().expect("the roster reads").len(), 1);
    }

    #[test]
    fn attach_ignores_a_refusal_addressed_to_another_request() {
        let scratch = Scratch::new("refusal-distractor");
        let (_fake, roster) = attached(
            &scratch,
            Behaviour {
                refusal_distractor: true,
                ..Behaviour::default()
            },
        );
        let roster = roster.expect("a refusal for another command is not ours");

        assert_eq!(roster.listed().expect("the roster reads").len(), 1);
    }

    #[test]
    fn attach_reports_the_daemons_refusal_of_the_hello() {
        let scratch = Scratch::new("hello-refused");
        let (_fake, roster) = attached(
            &scratch,
            Behaviour {
                hello_refusal: Some(Refusal::Malformed {
                    detail: "the hello was refused".to_owned(),
                }),
                ..Behaviour::default()
            },
        );
        let error = roster.err().expect("the daemon refused the hello");

        assert!(
            matches!(error, Error::Refused(Refusal::Malformed { .. })),
            "{error:?}",
        );
    }

    #[test]
    fn attach_reports_a_protocol_it_does_not_speak() {
        let scratch = Scratch::new("protocol");
        let (_fake, roster) = attached(
            &scratch,
            Behaviour {
                protocol: PROTOCOL + 1,
                ..Behaviour::default()
            },
        );
        let error = roster.err().expect("a newer daemon is refused");

        match error {
            Error::Refused(Refusal::Unsupported { protocol }) => {
                assert_eq!(protocol, PROTOCOL + 1);
            },
            other => panic!("the refusal names the protocol: {other:?}"),
        }
    }

    #[test]
    fn the_roster_reads_every_chunk_of_one_revision() {
        let scratch = Scratch::new("roster-chunks");
        let (_fake, roster) = attached(
            &scratch,
            Behaviour {
                roster_chunks: vec![
                    vec![entry(session(), "hold a conversation")],
                    vec![entry(another_session(), "count the quokkas")],
                ],
                ..Behaviour::default()
            },
        );
        let roster = roster.expect("the handshake answers");
        let listed = roster.listed().expect("the roster reads");

        assert_eq!(listed.len(), 2, "both chunks of the revision are listed");
        assert_eq!(listed[0].session, session());
        assert_eq!(listed[1].session, another_session());
    }

    #[test]
    fn a_new_roster_revision_replaces_the_listing() {
        let scratch = Scratch::new("roster-revision");
        let (fake, roster) = attached(&scratch, Behaviour::default());
        let roster = roster.expect("the handshake answers");
        assert_eq!(
            roster.listed().expect("the roster reads")[0].session,
            session(),
        );

        fake.set_roster(vec![entry(another_session(), "count the quokkas")]);
        let listed = wait_for_listing(&roster, |listed| {
            listed.len() == 1 && listed[0].session == another_session()
        });

        assert_eq!(listed[0].goal.as_deref(), Some("count the quokkas"));
    }

    #[test]
    fn a_session_opens_with_the_history_the_daemon_holds() {
        let scratch = Scratch::new("history");
        let (_fake, roster) = attached(&scratch, Behaviour::default());
        let roster = roster.expect("the handshake answers");
        let engine = roster.start(session()).expect("the history arrives");
        let fold = engine.fold().expect("the fold reads");

        assert_eq!(
            fold.task.as_ref().map(|task| task.goal.as_str()),
            Some("hold a conversation"),
        );
        assert_eq!(fold.turns.len(), 1);
    }

    #[test]
    fn a_history_chunk_out_of_sequence_replaces_the_fold_it_had() {
        let scratch = Scratch::new("history-restart");
        let (_fake, roster) = attached(
            &scratch,
            Behaviour {
                history: HistoryReply::Chunks(vec![
                    HistorySpec {
                        index: 0,
                        last: false,
                        seed: None,
                        records: opened_records(),
                    },
                    HistorySpec {
                        index: 0,
                        last: true,
                        seed: None,
                        records: Vec::new(),
                    },
                ]),
                ..Behaviour::default()
            },
        );
        let roster = roster.expect("the handshake answers");
        let engine = roster.start(session()).expect("the history arrives");
        let fold = engine.fold().expect("the fold reads");

        assert!(
            fold.turns.is_empty(),
            "the restart chunk is the whole of the history",
        );
        assert!(fold.task.is_none());
    }

    #[test]
    fn a_slow_history_still_opens_before_the_deadline() {
        let scratch = Scratch::new("history-slow");
        let (_fake, roster) = attached(
            &scratch,
            Behaviour {
                history: HistoryReply::Delayed(Duration::from_millis(200), opened_records()),
                ..Behaviour::default()
            },
        );
        let roster = roster.expect("the handshake answers");
        let engine = roster.start(session()).expect("the history arrives late");

        assert_eq!(engine.fold().expect("the fold reads").turns.len(), 1);
    }

    #[test]
    fn a_refused_history_is_the_errors_the_shell_prints() {
        let scratch = Scratch::new("history-refused");
        let (_fake, roster) = attached(
            &scratch,
            Behaviour {
                history: HistoryReply::Refused(Refusal::Untasked { session: session() }),
                ..Behaviour::default()
            },
        );
        let roster = roster.expect("the handshake answers");
        let error = roster
            .start(session())
            .err()
            .expect("the daemon refused the history");

        assert!(
            matches!(error, Error::Refused(Refusal::Untasked { .. })),
            "{error:?}",
        );
    }

    #[test]
    fn a_cancel_is_published_and_the_close_reaches_the_fold() {
        let scratch = Scratch::new("cancel");
        let (_fake, roster) = attached(&scratch, Behaviour::default());
        let roster = roster.expect("the handshake answers");
        let mut engine = roster.start(session()).expect("the history arrives");

        engine.cancel().expect("the cancel is published");
        let fold = wait_for_fold(&mut engine, |fold| {
            matches!(fold.session.phase(), Phase::Closed(Ending::Cancelled))
        });

        assert_eq!(fold.session.phase(), Phase::Closed(Ending::Cancelled));
        assert!(!engine.busy(), "the session is closed and owes nothing");
    }

    #[test]
    fn a_refused_cancel_is_a_screen_update() {
        let scratch = Scratch::new("cancel-refused");
        let (_fake, roster) = attached(
            &scratch,
            Behaviour {
                cancel: CancelReply::Refused(Refusal::Untasked { session: session() }),
                ..Behaviour::default()
            },
        );
        let roster = roster.expect("the handshake answers");
        let mut engine = roster.start(session()).expect("the history arrives");

        engine.cancel().expect("the cancel is published");
        let update = wait_for_update(&mut engine, |update| matches!(update, Update::Refused(_)));

        assert!(
            matches!(
                update,
                Update::Refused(Error::Refused(Refusal::Untasked { .. }))
            ),
            "{update:?}",
        );
    }

    #[test]
    fn a_spoken_line_is_published_and_its_record_comes_back() {
        let scratch = Scratch::new("speak");
        let (_fake, roster) = attached(&scratch, Behaviour::default());
        let roster = roster.expect("the handshake answers");
        let mut engine = roster.start(session()).expect("the history arrives");
        engine.speak("hello").expect("the line is published");
        let fold = wait_for_fold(&mut engine, |fold| {
            fold.speech(turn_id(session(), 1)) == Some("hello")
        });

        assert_eq!(fold.speech(turn_id(session(), 1)), Some("hello"));
    }

    #[test]
    fn a_refused_line_is_a_screen_update() {
        let scratch = Scratch::new("speak-refused");
        let (_fake, roster) = attached(
            &scratch,
            Behaviour {
                speak: SpeakReply::Refused(Refusal::NotAwaiting { session: session() }),
                ..Behaviour::default()
            },
        );
        let roster = roster.expect("the handshake answers");
        let mut engine = roster.start(session()).expect("the history arrives");
        engine.speak("hello").expect("the line is published");
        let update = wait_for_update(&mut engine, |update| matches!(update, Update::Refused(_)));

        assert!(
            matches!(
                update,
                Update::Refused(Error::Refused(Refusal::NotAwaiting { .. }))
            ),
            "{update:?}",
        );
    }

    #[test]
    fn dropping_the_roster_stops_its_connection() {
        let scratch = Scratch::new("drop");
        let fake = Fake::with_connections(&scratch, Behaviour::default(), 2);
        let first = BusRoster::attach(&fake.socket, Moment::EPOCH, scratch.checkout())
            .expect("the first roster answers");
        drop(first);

        let deadline = Instant::now() + PATIENCE;
        loop {
            match BusRoster::attach(&fake.socket, Moment::EPOCH, scratch.checkout()) {
                Ok(_second) => break,
                Err(error) => {
                    assert!(
                        Instant::now() < deadline,
                        "the dropped roster kept the only free connection: {error:?}",
                    );
                    std::thread::sleep(POLL);
                },
            }
        }
    }

    #[test]
    fn a_stopped_bus_reports_the_link_and_the_trouble() {
        let scratch = Scratch::new("reconnect");
        let (mut fake, roster) = attached(&scratch, Behaviour::default());
        let roster = roster.expect("the handshake answers");
        let mut engine = roster.start(session()).expect("the history arrives");

        // The history's ack has to land before the bus goes, or the reader stops on the ack's failure,
        // which reports nothing; the stop then has to find it parked on its next pull.
        std::thread::sleep(Duration::from_millis(200));
        fake.stop();

        let deadline = Instant::now() + PATIENCE;
        let mut seen = Vec::new();
        loop {
            seen.extend(engine.drain());
            if seen.iter().any(|update| matches!(update, Update::Link(_)))
                && seen
                    .iter()
                    .any(|update| matches!(update, Update::Trouble(_)))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the bus stopped silently: {seen:?}"
            );
            std::thread::sleep(POLL);
        }

        assert!(
            seen.iter()
                .any(|update| matches!(update, Update::Link(mokmok_tui::Link::Reconnecting))),
            "{seen:?}",
        );
        assert!(
            seen.iter().any(|update| matches!(
                update,
                Update::Trouble(detail) if detail.starts_with("the bus stopped:")
            )),
            "{seen:?}",
        );
    }

    struct Reaped(Child);

    impl Reaped {
        fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
            self.0.try_wait()
        }
    }

    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn binary(
        socket: &Path,
        session: &str,
    ) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mokmok-tui"));
        command
            .arg("--bus")
            .arg(socket)
            .arg("--session")
            .arg(session);

        command
    }

    fn on_a_pty(
        socket: &Path,
        session: &str,
    ) -> (Reaped, File, File) {
        let size = Winsize {
            ws_row: 10,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&size), None).expect("a pseudo-terminal is opened");
        let kept = pty.slave.try_clone().expect("the slave is kept");
        let input = pty
            .slave
            .try_clone()
            .expect("the slave is cloned for stdin");
        let master = File::from(pty.master);
        let flags = fcntl(&master, FcntlArg::F_GETFL).expect("the master's flags are read");
        fcntl(
            &master,
            FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
        )
        .expect("the master is made non-blocking");

        let child = binary(socket, session)
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(pty.slave))
            .stderr(Stdio::piped())
            .spawn()
            .expect("the binary runs");
        let child = Reaped(child);

        (child, master, File::from(kept))
    }

    fn drain(
        master: &File,
        transcript: &mut Vec<u8>,
    ) {
        let mut chunk = [0_u8; 4096];

        loop {
            match read(master.as_fd(), &mut chunk) {
                Ok(0) | Err(Errno::EAGAIN | Errno::EIO) => return,
                Ok(taken) => transcript.extend_from_slice(&chunk[..taken]),
                Err(errno) => panic!("the master could not be read: {errno}"),
            }
        }
    }

    fn found(
        hay: &[u8],
        needle: &[u8],
        from: usize,
    ) -> Option<usize> {
        hay.get(from..)?
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|at| at + from)
    }

    fn await_bytes(
        master: &File,
        transcript: &mut Vec<u8>,
        wanted: &[&[u8]],
        deadline: Instant,
    ) -> bool {
        loop {
            drain(master, transcript);

            if wanted
                .iter()
                .all(|needle| found(transcript, needle, 0).is_some())
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }

            std::thread::sleep(POLL);
        }
    }

    fn await_exit(
        child: &mut Reaped,
        master: &File,
        transcript: &mut Vec<u8>,
        deadline: Instant,
    ) -> ExitStatus {
        loop {
            drain(master, transcript);

            if let Some(status) = child.try_wait().expect("the child is asked about") {
                drain(master, transcript);

                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the child had not exited: {}",
                transcript.escape_ascii(),
            );
            std::thread::sleep(POLL);
        }
    }

    #[test]
    fn a_conversation_on_a_terminal_draws_a_frame_and_hands_the_shell_back() {
        let scratch = Scratch::new("pty");
        let fake = Fake::start(
            &scratch,
            Behaviour {
                history: HistoryReply::Folded(working_records()),
                ..Behaviour::default()
            },
        );
        let (mut child, mut master, kept) =
            on_a_pty(&fake.socket, &session().as_uuid().to_string());
        let deadline = Instant::now() + PATIENCE;
        let mut transcript = Vec::new();

        assert!(
            await_bytes(&master, &mut transcript, &[b"mokmok"], deadline),
            "the frame is drawn: {}",
            transcript.escape_ascii(),
        );
        assert!(
            await_bytes(
                &master,
                &mut transcript,
                &[b"the agent is working"],
                deadline,
            ),
            "the status follows the session in flight: {}",
            transcript.escape_ascii(),
        );

        // The burst leaves keys in the terminal's buffer, so a poll that answers without reading them
        // cannot be followed by the one character that proves the keys were taken.
        master
            .write_all(b"aaaaaaaaaa")
            .expect("a burst is typed while the turn runs");
        std::thread::sleep(Duration::from_millis(50));
        master.write_all(b"Z").expect("the last character is typed");
        assert!(
            await_bytes(&master, &mut transcript, &[b"Z"], deadline),
            "a key typed during the turn is drawn: {}",
            transcript.escape_ascii(),
        );

        let raw = tcgetattr(&kept)
            .expect("the slave's settings are read")
            .local_flags;
        assert!(
            !raw.contains(LocalFlags::ICANON | LocalFlags::ECHO),
            "the conversation's screen is raw: {raw:?}",
        );

        master.write_all(CTRL_C).expect("the interrupt is typed");
        let status = await_exit(&mut child, &master, &mut transcript, deadline);
        assert_eq!(
            status.code(),
            Some(0),
            "the conversation leaves when the human does: {}",
            transcript.escape_ascii(),
        );

        let leaving = found(&transcript, LEAVE_ALTERNATE, 0).unwrap_or_else(|| {
            panic!(
                "the child leaves the alternate screen: {}",
                transcript.escape_ascii(),
            )
        });
        assert!(
            found(&transcript, SHOW_CURSOR, leaving).is_some(),
            "the cursor comes back after the alternate screen: {}",
            transcript.escape_ascii(),
        );
        let settings = tcgetattr(&kept)
            .expect("the slave's settings are read")
            .local_flags;
        assert!(
            settings.contains(LocalFlags::ICANON),
            "canonical input is back: {settings:?}",
        );
        assert!(
            settings.contains(LocalFlags::ECHO),
            "echo is back: {settings:?}",
        );
    }

    #[test]
    fn a_run_whose_input_is_a_pipe_fails_instead_of_drawing() {
        let scratch = Scratch::new("pipe");
        let fake = Fake::start(&scratch, Behaviour::default());

        let run = binary(&fake.socket, &session().as_uuid().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("the binary runs");
        let stderr = String::from_utf8(run.stderr).expect("the message is text");

        assert_eq!(
            run.status.code(),
            Some(1),
            "a run that cannot draw a screen fails: {stderr}",
        );
        assert!(
            stderr.starts_with("mokmok-tui: this is not a terminal"),
            "the failure names what is missing: {stderr}",
        );
    }

    #[test]
    fn an_invalid_session_fails_before_the_screen() {
        let scratch = Scratch::new("invalid-session");
        let fake = Fake::start(&scratch, Behaviour::default());

        let run = binary(&fake.socket, "not-a-session")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("the binary runs");
        let stderr = String::from_utf8(run.stderr).expect("the message is text");

        assert_eq!(
            run.status.code(),
            Some(1),
            "a session the daemon cannot name fails: {stderr}",
        );
        assert!(
            stderr.starts_with("mokmok-tui: not-a-session is not a session UUID"),
            "the failure names the value: {stderr}",
        );
    }
}
