//! The daemon under a real supervisor: the built binary, a real notify datagram socket, and real
//! signals.
//!
//! Every assertion is on something an operator can observe: the bytes the supervisor's socket
//! received, the exit status, the lines the daemon printed, and whether the bus socket is still
//! there. Nothing reaches into the kernel's own types to decide what happened.
//!
//! The harness lives inside a `#[cfg(test)]` module because that is what tells the workspace's
//! clippy settings that this is test code, where `expect` and `panic!` are allowed.

#[cfg(test)]
mod tests {
    use std::os::linux::net::SocketAddrExt as _;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus, Output, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use mokmok_event_bus::{Subscriber, SubscriberId, Topic};
    use mokmok_event_store::{CommandId, Expected};
    use mokmok_kernel::{Db, Entry, Notice};
    use mokmok_protocol::{Moment, SessionID};
    use mokmok_state_machine::{Fact, Resolution};
    use uuid::Uuid;

    const SESSION: &str = "018f2f1e-0000-7000-8000-000000000000";

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn binary() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_mokmokd"))
    }

    fn session() -> SessionID {
        SessionID::from_uuid(Uuid::parse_str(SESSION).expect("the session is a uuid"))
    }

    fn session_of(seed: u128) -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(seed))
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let pid = std::process::id();
            let path = std::env::temp_dir().join(format!("mokmok-supervisor-{name}-{pid}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();

            Self(path)
        }

        fn data(&self) -> PathBuf {
            self.0.join("data")
        }

        fn socket(&self) -> PathBuf {
            self.0.join("bus.sock")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Writes `count` pending notices for the session, the durable state a previous instance left
    /// in flight for the bus to relay.
    fn seed(
        data: &Path,
        first: u128,
        count: u128,
    ) {
        seed_for(data, session(), first, count);
    }

    /// Writes `count` pending notices for `session`, whose topic the relay publishes to.
    fn seed_for(
        data: &Path,
        session: SessionID,
        first: u128,
        count: u128,
    ) {
        std::fs::create_dir_all(data).unwrap();
        let mut db = Db::open(&data.join("events.sqlite")).unwrap();

        for index in first..first + count {
            let command = CommandId::from_uuid(Uuid::from_u128(index));
            let notice = Notice {
                session,
                command,
                turn: None,
                execution: None,
                resolution: Some(Resolution::Unknown),
                at: Moment::from_millis(1),
            };
            db.append(
                command,
                Db::stream(session),
                Expected::Any,
                &[Entry::Session(Fact::SessionOpened)],
                vec![notice.outbox().unwrap()],
            )
            .unwrap();
        }
    }

    struct Ran {
        code: Option<i32>,
        out: String,
        err: String,
    }

    fn exited(
        child: &mut Child,
        bound: Duration,
    ) -> Option<ExitStatus> {
        let deadline = Instant::now() + bound;

        loop {
            if let Some(status) = child.try_wait().expect("the child can be polled") {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn ran(output: &Output) -> Ran {
        Ran {
            code: output.status.code(),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// The shared command line, with the confiner pointed at the daemon itself: it is resolved at
    /// startup and never run on the serving path, so this keeps the test independent of whether the
    /// rest of the workspace was built.
    fn base(scratch: &Scratch) -> Command {
        let mut command = Command::new(binary());
        command
            .args([
                "--data-dir".to_owned(),
                scratch.data().display().to_string(),
                "--confiner".to_owned(),
                binary().display().to_string(),
            ])
            .env_remove("NOTIFY_SOCKET")
            .env_remove("WATCHDOG_USEC")
            .env_remove("WATCHDOG_PID")
            .env_remove("RUST_LOG");

        command
    }

    struct Daemon {
        child: Option<Child>,
        pid: i32,
    }

    impl Daemon {
        fn start(
            scratch: &Scratch,
            environment: &[(&str, &str)],
        ) -> Self {
            Self::spawn(scratch, environment, &[])
        }

        fn spawn(
            scratch: &Scratch,
            environment: &[(&str, &str)],
            extra: &[&str],
        ) -> Self {
            let mut command = base(scratch);
            command
                .args(["serve".to_owned()])
                .args(extra.iter().map(|value| (*value).to_owned()))
                .args([
                    "--socket".to_owned(),
                    scratch.socket().display().to_string(),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (key, value) in environment {
                command.env(key, value);
            }

            let child = command.spawn().expect("the daemon starts");
            let pid = i32::try_from(child.id()).expect("a pid fits an i32");

            Self {
                child: Some(child),
                pid,
            }
        }

        fn signal(
            &self,
            signal: &str,
        ) {
            let status = Command::new("sh")
                .arg("-c")
                .arg(format!("kill -{signal} {}", self.pid))
                .status()
                .expect("the shell runs");

            assert!(status.success(), "kill -{signal} succeeded");
        }

        fn kill(mut self) {
            let mut child = self.child.take().expect("the daemon is running");
            child.kill().expect("the daemon is killed");
            child.wait().expect("the daemon is reaped");
        }

        fn finish(
            mut self,
            bound: Duration,
        ) -> Ran {
            let mut child = self.child.take().expect("the daemon is running");
            if exited(&mut child, bound).is_none() {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the daemon did not exit within {bound:?}");
            }

            ran(&child.wait_with_output().expect("the output is read"))
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                // A process frozen by a failed assertion is still killable, so a test that panics
                // between `STOP` and `CONT` leaves nothing behind.
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    struct Supervisor {
        name: String,
        socket: UnixDatagram,
    }

    impl Supervisor {
        fn bind(tag: &str) -> Self {
            let name = format!(
                "/mokmok-supervisor-{}-{}-{tag}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            );
            let address = SocketAddr::from_abstract_name(name.as_str()).unwrap();
            let socket = UnixDatagram::bind_addr(&address).unwrap();

            Self { name, socket }
        }

        /// The `NOTIFY_SOCKET` value systemd itself sets: `@`-prefixed for the abstract namespace.
        fn variable(&self) -> String {
            format!("@{}", self.name)
        }

        fn read(
            &self,
            bound: Duration,
        ) -> Option<String> {
            self.socket.set_read_timeout(Some(bound)).unwrap();
            let mut buffer = [0_u8; 256];
            match self.socket.recv(&mut buffer) {
                Ok(read) => Some(String::from_utf8_lossy(&buffer[..read]).into_owned()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    None
                },
                Err(error) => panic!("the notify socket failed: {error}"),
            }
        }

        fn expect(
            &self,
            bound: Duration,
            wanted: &str,
        ) {
            let read = self
                .read(bound)
                .unwrap_or_else(|| panic!("no {wanted:?} arrived within {bound:?}"));

            assert_eq!(read, wanted);
        }

        fn silent(
            &self,
            bound: Duration,
        ) -> bool {
            loop {
                if self.read(Duration::from_millis(30)).is_none() {
                    break;
                }
            }

            self.read(bound).is_none()
        }
    }

    fn wait_for_socket(
        path: &Path,
        bound: Duration,
    ) {
        let deadline = Instant::now() + bound;
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "the bus bound {} within {bound:?}",
                path.display(),
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn collect(
        socket: &Path,
        topic: Topic,
        subscriber: SubscriberId,
        want: usize,
    ) -> Vec<String> {
        runtime().block_on(async {
            let mut subscriber = tokio::time::timeout(
                Duration::from_secs(5),
                Subscriber::open(socket, topic, subscriber),
            )
            .await
            .expect("the subscriber connects within five seconds")
            .expect("the bus accepts the session");
            let mut seen = Vec::new();
            while seen.len() < want {
                let Ok(Ok(batch)) =
                    tokio::time::timeout(Duration::from_secs(5), subscriber.next()).await
                else {
                    break;
                };
                seen.extend(
                    batch
                        .events()
                        .iter()
                        .map(|event| event.payload().as_str().to_owned()),
                );
                subscriber.ack(batch.after()).await.expect("the ack lands");
            }

            seen
        })
    }

    fn once(
        scratch: &Scratch,
        environment: &[(&str, &str)],
    ) -> Ran {
        let mut command = base(scratch);
        command
            .args([
                "serve".to_owned(),
                "--once".to_owned(),
                "--socket".to_owned(),
                scratch.socket().display().to_string(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in environment {
            command.env(key, value);
        }

        ran(&command.output().expect("the daemon runs"))
    }

    #[test]
    fn ready_is_announced_only_after_the_bus_answers_and_the_drain_returned() {
        let scratch = Scratch::new("ready");
        seed(&scratch.data(), 0x11, 4);
        let supervisor = Supervisor::bind("ready");
        let notify = supervisor.variable();
        let daemon = Daemon::start(&scratch, &[("NOTIFY_SOCKET", notify.as_str())]);

        supervisor.expect(Duration::from_secs(5), "READY=1\n");
        assert!(
            supervisor.read(Duration::from_millis(300)).is_none(),
            "readiness is announced once, and nothing follows it before the stop",
        );

        let notices = collect(
            &scratch.socket(),
            Topic::Session(session()),
            SubscriberId::new(),
            4,
        );
        assert_eq!(
            notices.len(),
            4,
            "every pending notice was relayed: {notices:#?}",
        );
        assert!(
            notices.iter().all(|notice| notice.contains(SESSION)),
            "every notice names the session it belongs to: {notices:#?}",
        );

        daemon.signal("TERM");
        supervisor.expect(Duration::from_secs(5), "STOPPING=1\n");
        let finished = daemon.finish(Duration::from_secs(5));

        assert_eq!(finished.code, Some(0), "{}", finished.err);
        assert_eq!(
            finished.out,
            format!("published=4 socket={}\n", scratch.socket().display()),
            "readiness was gated on the pass that published all four",
        );
    }

    #[test]
    fn a_socket_a_foreign_process_holds_is_never_reported_ready() {
        let scratch = Scratch::new("foreign");
        seed(&scratch.data(), 0x31, 1);

        let listener = std::os::unix::net::UnixListener::bind(scratch.socket()).unwrap();
        std::thread::spawn(move || while listener.accept().is_ok() {});

        let supervisor = Supervisor::bind("foreign");
        let notify = supervisor.variable();
        let daemon = Daemon::start(&scratch, &[("NOTIFY_SOCKET", notify.as_str())]);
        let finished = daemon.finish(Duration::from_secs(3));

        assert_eq!(finished.code, Some(1), "{}\n{}", finished.out, finished.err);
        assert!(
            finished.err.contains("already serving"),
            "the refusal that ended the daemon is printed: {}",
            finished.err,
        );
        assert!(
            supervisor.read(Duration::from_millis(300)).is_none(),
            "a bus that never bound was never reported ready",
        );
    }

    #[test]
    fn a_stop_drains_and_exits_zero_without_leaving_the_socket() {
        let scratch = Scratch::new("stop");
        seed(&scratch.data(), 0x21, 4);
        let supervisor = Supervisor::bind("stop");
        let notify = supervisor.variable();
        let daemon = Daemon::start(&scratch, &[("NOTIFY_SOCKET", notify.as_str())]);

        supervisor.expect(Duration::from_secs(5), "READY=1\n");
        let notices = collect(
            &scratch.socket(),
            Topic::Session(session()),
            SubscriberId::new(),
            4,
        );
        assert_eq!(
            notices.len(),
            4,
            "the pending notices were relayed before the stop: {notices:#?}",
        );

        daemon.signal("TERM");
        supervisor.expect(Duration::from_secs(5), "STOPPING=1\n");
        let finished = daemon.finish(Duration::from_secs(5));

        assert_eq!(finished.code, Some(0), "{}", finished.err);
        assert!(
            !scratch.socket().exists(),
            "the bus unlinked its socket on the way out",
        );
    }

    #[test]
    fn a_kill_nine_restart_serves_the_same_log_over_the_stale_socket() {
        let scratch = Scratch::new("restart");
        seed(&scratch.data(), 0x41, 4);

        let first = Supervisor::bind("restart-first");
        let notify = first.variable();
        let daemon = Daemon::start(&scratch, &[("NOTIFY_SOCKET", notify.as_str())]);
        first.expect(Duration::from_secs(5), "READY=1\n");

        let subscriber = SubscriberId::new();
        let before = collect(&scratch.socket(), Topic::Session(session()), subscriber, 4);
        assert_eq!(
            before.len(),
            4,
            "the first instance relayed every pending notice: {before:#?}",
        );

        daemon.kill();
        assert!(
            scratch.socket().exists(),
            "a process killed with SIGKILL leaves its socket file behind",
        );

        seed(&scratch.data(), 0x45, 2);

        let second = Supervisor::bind("restart-second");
        let notify = second.variable();
        let replacement = Daemon::start(&scratch, &[("NOTIFY_SOCKET", notify.as_str())]);
        second.expect(Duration::from_secs(5), "READY=1\n");

        let after = collect(&scratch.socket(), Topic::Session(session()), subscriber, 2);
        assert_eq!(
            after.len(),
            2,
            "the subscriber resumed from its checkpoint instead of replaying the log: {after:#?}",
        );
        assert!(
            after.iter().all(|notice| {
                notice.contains(&Uuid::from_u128(0x45).to_string())
                    || notice.contains(&Uuid::from_u128(0x46).to_string())
            }),
            "the resumed subscriber received only what was written after the kill: {after:#?}",
        );

        replacement.signal("TERM");
        second.expect(Duration::from_secs(5), "STOPPING=1\n");
        let finished = replacement.finish(Duration::from_secs(5));

        assert_eq!(finished.code, Some(0), "{}", finished.err);
        assert_eq!(
            finished.out,
            format!("published=2 socket={}\n", scratch.socket().display()),
            "the replacement served the same database and drained only what was pending",
        );
    }

    #[test]
    fn the_keep_alive_follows_the_relay_and_stops_when_the_process_does() {
        let scratch = Scratch::new("watchdog");
        let supervisor = Supervisor::bind("watchdog");
        let notify = supervisor.variable();
        let daemon = Daemon::start(
            &scratch,
            &[
                ("NOTIFY_SOCKET", notify.as_str()),
                ("WATCHDOG_USEC", "400000"),
            ],
        );

        supervisor.expect(Duration::from_secs(5), "READY=1\n");
        supervisor.expect(Duration::from_secs(2), "WATCHDOG=1\n");

        daemon.signal("STOP");
        std::thread::sleep(Duration::from_millis(250));
        assert!(
            supervisor.silent(Duration::from_millis(900)),
            "a frozen relay stops keeping the watchdog fed",
        );

        daemon.signal("CONT");
        supervisor.expect(Duration::from_secs(2), "WATCHDOG=1\n");

        daemon.signal("TERM");
        supervisor.expect(Duration::from_secs(5), "STOPPING=1\n");
        let finished = daemon.finish(Duration::from_secs(5));

        assert_eq!(finished.code, Some(0), "{}", finished.err);
    }

    #[test]
    fn a_watchdog_set_for_another_process_is_not_this_processes_deadline() {
        let scratch = Scratch::new("other-watchdog");
        let supervisor = Supervisor::bind("other-watchdog");
        let notify = supervisor.variable();
        let daemon = Daemon::start(
            &scratch,
            &[
                ("NOTIFY_SOCKET", notify.as_str()),
                ("WATCHDOG_USEC", "400000"),
                ("WATCHDOG_PID", "1"),
            ],
        );

        supervisor.expect(Duration::from_secs(5), "READY=1\n");
        assert!(
            supervisor.read(Duration::from_millis(900)).is_none(),
            "a watchdog aimed at another process is not a reason to report liveness",
        );

        daemon.signal("TERM");
        supervisor.expect(Duration::from_secs(5), "STOPPING=1\n");
        let finished = daemon.finish(Duration::from_secs(5));

        assert_eq!(finished.code, Some(0), "{}", finished.err);
    }

    #[test]
    fn with_no_supervisor_the_daemon_serves_and_stops_the_same() {
        let scratch = Scratch::new("unsupervised");
        seed(&scratch.data(), 0x71, 4);
        let daemon = Daemon::start(&scratch, &[]);

        wait_for_socket(&scratch.socket(), Duration::from_secs(5));
        let notices = collect(
            &scratch.socket(),
            Topic::Session(session()),
            SubscriberId::new(),
            4,
        );
        assert_eq!(
            notices.len(),
            4,
            "the daemon served without a supervisor: {notices:#?}",
        );

        daemon.signal("TERM");
        let finished = daemon.finish(Duration::from_secs(5));

        assert_eq!(finished.code, Some(0), "{}", finished.err);
        assert_eq!(
            finished.out,
            format!("published=4 socket={}\n", scratch.socket().display()),
        );
        assert!(!scratch.socket().exists());
    }

    #[test]
    fn once_drains_the_pending_outbox_and_reports_nothing_to_a_supervisor() {
        let scratch = Scratch::new("once");
        seed(&scratch.data(), 0x81, 4);
        let supervisor = Supervisor::bind("once");
        let notify = supervisor.variable();

        let stale = std::os::unix::net::UnixListener::bind(scratch.socket()).unwrap();
        drop(stale);
        assert!(
            scratch.socket().exists(),
            "the stale socket file is left behind"
        );

        let first = once(&scratch, &[("NOTIFY_SOCKET", notify.as_str())]);
        assert_eq!(first.code, Some(0), "{}", first.err);
        assert_eq!(
            first.out,
            format!("published=4 socket={}\n", scratch.socket().display()),
        );
        assert!(
            supervisor.read(Duration::from_millis(200)).is_none(),
            "--once has no readiness to announce",
        );

        let second = once(&scratch, &[]);
        assert_eq!(second.code, Some(0), "{}", second.err);
        assert_eq!(
            second.out,
            format!("published=0 socket={}\n", scratch.socket().display()),
            "the first run drained the outbox",
        );
    }

    #[test]
    fn a_peer_that_accepts_and_never_answers_fails_the_start_instead_of_hanging() {
        let scratch = Scratch::new("silent-peer");
        seed(&scratch.data(), 0x91, 1);

        let listener = std::os::unix::net::UnixListener::bind(scratch.socket()).unwrap();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept() {
                held.push(stream);
            }
        });

        let supervisor = Supervisor::bind("silent-peer");
        let notify = supervisor.variable();
        let daemon = Daemon::spawn(&scratch, &[("NOTIFY_SOCKET", notify.as_str())], &["--once"]);

        let started = Instant::now();
        let finished = daemon.finish(Duration::from_secs(12));
        let elapsed = started.elapsed();

        assert_eq!(finished.code, Some(1), "{}\n{}", finished.out, finished.err);
        assert!(
            elapsed < Duration::from_secs(12),
            "the start ended a few seconds past BIND rather than never: {elapsed:?}",
        );
        assert!(
            finished.err.contains("never answered"),
            "the deadline that ended the start is named: {}",
            finished.err,
        );
        assert!(
            supervisor.read(Duration::from_millis(200)).is_none(),
            "a peer that never answered was never reported ready",
        );
    }

    #[test]
    fn a_stop_is_honoured_while_a_publish_goes_unanswered() {
        let scratch = Scratch::new("wedged-publish");
        let supervisor = Supervisor::bind("wedged-publish");
        let notify = supervisor.variable();
        let daemon = Daemon::start(&scratch, &[("NOTIFY_SOCKET", notify.as_str())]);
        supervisor.expect(Duration::from_secs(5), "READY=1\n");

        // The path every later pass will dial: a peer that accepts and answers nothing, so the pass
        // the stop lands in is a pass that cannot finish on its own.
        std::fs::remove_file(scratch.socket()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(scratch.socket()).unwrap();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept() {
                held.push(stream);
            }
        });

        // One notice per topic, so a pass that had to wait a bound per notice would be slow to cut.
        for index in 0..4 {
            seed_for(&scratch.data(), session_of(0xb0 + index), 0xb0 + index, 1);
        }
        std::thread::sleep(Duration::from_millis(300));

        let started = Instant::now();
        daemon.signal("TERM");
        let finished = daemon.finish(Duration::from_secs(10));

        assert_eq!(finished.code, Some(0), "{}\n{}", finished.out, finished.err);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "a stop cuts the pass in flight instead of waiting it out: {:?}",
            started.elapsed(),
        );
        assert!(
            !scratch.socket().exists(),
            "the daemon's own bus unlinked the socket",
        );
        assert_eq!(
            Db::open(&scratch.data().join("events.sqlite"))
                .unwrap()
                .pending()
                .unwrap()
                .len(),
            4,
            "a cut pass leaves every row pending rather than losing or settling it",
        );
        assert_eq!(
            finished.out,
            format!("published=0 socket={}\n", scratch.socket().display()),
            "nothing was acknowledged",
        );
    }

    #[test]
    fn the_bus_log_reaches_stdout_when_the_operator_turns_it_on() {
        let scratch = Scratch::new("info-log");
        seed(&scratch.data(), 0xb1, 1);

        let daemon = Daemon::start(&scratch, &[("RUST_LOG", "info")]);
        wait_for_socket(&scratch.socket(), Duration::from_secs(5));
        let notices = collect(
            &scratch.socket(),
            Topic::Session(session()),
            SubscriberId::new(),
            1,
        );
        assert_eq!(
            notices.len(),
            1,
            "the seeded notice was relayed, which is what opens the session's topic: {notices:#?}",
        );

        daemon.signal("TERM");
        let finished = daemon.finish(Duration::from_secs(5));

        assert_eq!(finished.code, Some(0), "{}", finished.err);
        // The line is JSON whose target and topic vary with the bus's internals, so only the
        // message field is asserted, not the whole line.
        assert!(
            finished.out.contains("opened a topic"),
            "the bus's info line reached the daemon's stdout: {}",
            finished.out,
        );
    }

    #[test]
    fn a_subscriber_that_cannot_be_installed_is_reported_without_failing_the_daemon() {
        let scratch = Scratch::new("bad-log-filter");
        seed(&scratch.data(), 0xc1, 1);

        // A directive whose level is not a level is the one installation failure a test can
        // reach, because a fresh process has no global subscriber to collide with.
        let daemon = Daemon::start(&scratch, &[("RUST_LOG", "mokmok=nonsense")]);
        wait_for_socket(&scratch.socket(), Duration::from_secs(5));
        let notices = collect(
            &scratch.socket(),
            Topic::Session(session()),
            SubscriberId::new(),
            1,
        );
        assert_eq!(
            notices.len(),
            1,
            "the daemon served despite the filter that could not be installed: {notices:#?}",
        );

        daemon.signal("TERM");
        let finished = daemon.finish(Duration::from_secs(5));

        assert_eq!(finished.code, Some(0), "{}", finished.err);
        assert_eq!(
            finished.out,
            format!("published=1 socket={}\n", scratch.socket().display()),
            "no subscriber means no bus line on the daemon's stdout",
        );
        assert!(
            finished.err.contains("log subscriber was not installed"),
            "the failure to install is reported on stderr: {}",
            finished.err,
        );
    }
}
