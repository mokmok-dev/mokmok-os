//! The terminal's bus adapter against a real daemon and a real bus.
//!
//! This is the seam the change is about: the daemon owns the session, the bus carries the
//! records, and the terminal folds them. No local log and no in-process driver is involved.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use mokmok_agent_runtime::{ContextAct, Transcript};
    use mokmok_protocol::Moment;
    use mokmok_tui::{BusRoster, Engine, Roster};

    const PATIENCE: Duration = Duration::from_secs(10);
    const POLL: Duration = Duration::from_millis(10);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mokmok-tui-bus-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join("checkout")).expect("the scratch checkout is made");

            Self(path)
        }

        fn data(&self) -> PathBuf {
            self.0.join("data")
        }

        fn checkout(&self) -> PathBuf {
            std::fs::canonicalize(self.0.join("checkout")).expect("the checkout is real")
        }

        fn socket(&self) -> PathBuf {
            self.0.join("bus.sock")
        }

        fn script(&self) -> PathBuf {
            self.0.join("script.json")
        }

        fn log(&self) -> PathBuf {
            self.0.join("daemon.log")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            if std::thread::panicking()
                && let Ok(log) = std::fs::read_to_string(self.log())
            {
                eprintln!("--- daemon {} ---\n{log}", self.0.display());
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The daemon this package's test build produced beside the terminal binary.
    ///
    /// A package-scoped build leaves it out, the same way `mokmok-sandbox-confine` is left out, so the
    /// case returns early instead of failing a build that never promised the binary.
    fn daemon() -> Option<PathBuf> {
        if let Some(named) = std::env::var_os("MOKMOK_DAEMON") {
            let path = PathBuf::from(named);

            return path.is_file().then_some(path);
        }

        PathBuf::from(env!("CARGO_BIN_EXE_mokmok-tui"))
            .parent()
            .map(|directory| directory.join("mokmokd"))
            .filter(|path| path.is_file())
    }

    /// The daemon, killed and reaped when the case ends, including a case that panics.
    struct Daemon(Child);

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn start(
        daemon: &Path,
        scratch: &Scratch,
    ) -> Daemon {
        std::fs::write(scratch.script(), br#"[{"action": "final", "say": "done"}]"#)
            .expect("the script is written");

        Daemon(
            Command::new(daemon)
        // No operator configuration file: the script is the only provider this case is about, and
        // the file would otherwise choose a real endpoint.
        .env("XDG_CONFIG_HOME", scratch.0.join("config"))
        .args([
            "--data-dir".to_owned(),
            scratch.data().display().to_string(),
            "--confiner".to_owned(),
            "/bin/sh".to_owned(),
            "--allow-tool".to_owned(),
            "/bin/sh".to_owned(),
            "--allow-path".to_owned(),
            "/nix/store".to_owned(),
            "--allow-write".to_owned(),
            scratch.checkout().display().to_string(),
            "--script".to_owned(),
            scratch.script().display().to_string(),
            "serve".to_owned(),
            "--socket".to_owned(),
            scratch.socket().display().to_string(),
        ])
        .stdout(Stdio::null())
        // A daemon that outlives a panicking case must not hold the test harness's stderr pipe
        // open, or the harness waits for a writer that never ends.
        .stderr(Stdio::from(
            std::fs::File::create(scratch.log()).expect("the daemon log is made"),
        ))
        .spawn()
        .expect("the daemon starts"),
        )
    }

    fn wait_for_socket(path: &Path) {
        let deadline = Instant::now() + PATIENCE;

        while Instant::now() < deadline {
            if path.exists() {
                return;
            }
            std::thread::sleep(POLL);
        }

        panic!("the bus never bound {}", path.display());
    }

    /// Drains the engine until `done` holds for its fold, then answers the fold.
    fn wait_for(
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
            let shown: Vec<String> = fold
                .shown_turns()
                .into_iter()
                .map(|(_, turn)| format!("{:?}/{:?}", turn.speech.as_deref(), turn.act))
                .collect();
            assert!(
                Instant::now() < deadline,
                "the session never reached the expected state: phase={:?} task={} turns={} shown={shown:?}",
                fold.session.phase(),
                fold.task.is_some(),
                fold.turns.len(),
            );
            std::thread::sleep(POLL);
        }
    }

    fn answered(fold: &Transcript) -> bool {
        fold.shown_turns().into_iter().any(
            |(_, turn)| matches!(turn.act, Some(ContextAct::Said { ref reply }) if reply == "done"),
        )
    }

    #[test]
    fn a_terminal_creates_a_session_speaks_and_reads_the_daemons_answer() {
        let Some(daemon) = daemon() else {
            return;
        };
        let scratch = Scratch::new("answer");
        let socket = scratch.socket();
        let _daemon = start(&daemon, &scratch);
        wait_for_socket(&socket);

        let at = Moment::from_millis(1_700_000_000_000);
        let roster = BusRoster::attach(&socket, at, scratch.checkout()).expect("the bus answers");
        let mut engine = roster
            .create("hold a conversation")
            .expect("a session is seeded");
        let _session = engine.session();

        let seeded = wait_for(&mut engine, |fold| fold.task.is_some());
        assert_eq!(seeded.task.as_ref().unwrap().goal, "hold a conversation");

        engine.speak("hello").expect("the line is published");
        let fold = wait_for(&mut engine, answered);
        assert!(answered(&fold));
    }

    #[test]
    fn a_restarted_terminal_reads_the_same_transcript_from_the_daemon() {
        let Some(daemon) = daemon() else {
            return;
        };
        let scratch = Scratch::new("restart");
        let socket = scratch.socket();
        let _daemon = start(&daemon, &scratch);
        wait_for_socket(&socket);

        let at = Moment::from_millis(1_700_000_000_000);
        let first = BusRoster::attach(&socket, at, scratch.checkout()).expect("the bus answers");
        let mut engine = first
            .create("hold a conversation")
            .expect("a session is seeded");
        let session = engine.session();
        let _ = wait_for(&mut engine, |fold| fold.task.is_some());
        engine.speak("hello").expect("the line is published");
        let _ = wait_for(&mut engine, answered);
        drop(engine);
        drop(first);

        let second = BusRoster::attach(&socket, at, scratch.checkout()).expect("the bus answers");
        let mut resumed = second.start(session).expect("the session opens");
        let fold = wait_for(&mut resumed, answered);
        assert!(
            fold.shown_turns().into_iter().any(|(_, turn)| {
                matches!(turn.act, Some(ContextAct::Said { ref reply }) if reply == "done")
            }),
            "the transcript came from the daemon, not from this process",
        );
    }
}
