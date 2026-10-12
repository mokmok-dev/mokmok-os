//! The vertical slice, driven the way a caller drives it: through `mokmokd`, against a real
//! SQLite log, a real confined process, and a real bus socket.
//!
//! Every assertion here is on something a caller can observe: the exit status, the lines the
//! binary prints, the bytes the artifact store kept, and whether the supervisor left an execution
//! directory behind. Nothing reaches into the kernel's own types to decide whether the work
//! happened.
//!
//! The harness lives inside a `#[cfg(test)]` module because that is what tells the workspace's
//! clippy settings that this is test code, where `expect` and `panic!` are allowed.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};

    use mokmok_event_bus::{Subscriber, SubscriberId, Topic};

    /// The SHA-256 of the empty input, as a known-answer vector from outside this code.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    /// The SHA-256 of `hello`, likewise.
    const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    const SESSION: &str = "018f2f1e-0000-7000-8000-000000000000";
    const KEY_A: &str = "018f2f1e-0000-7000-8000-0000000000a1";
    const KEY_B: &str = "018f2f1e-0000-7000-8000-0000000000b2";
    const KEY_CRASH_EARLY: &str = "018f2f1e-0000-7000-8000-0000000000c3";
    const KEY_CRASH_LATE: &str = "018f2f1e-0000-7000-8000-0000000000d4";

    /// A directory of this test's own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mokmok-slice-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);

            Self(path)
        }

        fn data(&self) -> PathBuf {
            self.0.join("data")
        }

        fn sandbox_root(&self) -> PathBuf {
            self.data().join("sandbox")
        }

        /// How many execution directories the supervisor left behind.
        fn started(&self) -> usize {
            std::fs::read_dir(self.sandbox_root()).map_or(0, std::fs::ReadDir::count)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One finished invocation of the binary.
    struct Ran {
        ok: bool,
        code: Option<i32>,
        out: String,
        err: String,
    }

    impl Ran {
        fn value(
            &self,
            key: &str,
        ) -> Option<String> {
            self.out.lines().find_map(|line| {
                line.split_whitespace()
                    .find_map(|field| field.strip_prefix(&format!("{key}=")).map(str::to_owned))
            })
        }

        /// The whole line that starts with `head`, which is how a value holding spaces is read.
        fn phrase(
            &self,
            head: &str,
        ) -> Option<&str> {
            self.out.lines().find(|line| line.starts_with(head))
        }

        fn require(
            &self,
            head: &str,
        ) -> &str {
            self.phrase(head)
                .unwrap_or_else(|| panic!("no {head} in:\n{}\n{}", self.out, self.err))
        }
    }

    fn binary() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_mokmokd"))
    }

    fn confiner() -> PathBuf {
        let path = binary()
            .parent()
            .expect("the binary has a directory")
            .join("mokmok-sandbox-confine");
        // The confiner belongs to another package, and cargo builds no target of another package
        // for this one's tests. `cargo test --workspace` and the nix check both build it; a test run
        // narrowed to this package in a fresh target directory does not, and the failure it caused
        // would otherwise read as a misconfiguration rather than a missing build.
        assert!(
            path.is_file(),
            "{} is missing: build the workspace (cargo build --workspace) before running this \
             package's tests alone",
            path.display(),
        );

        path
    }

    /// The first shell on `PATH`, which is where a confined workload can find one.
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

        panic!("no shell on PATH, so the slice cannot run a tool");
    }

    /// The directories the workload may read and execute in: the store every nix-provided binary and
    /// library lives in, and the system directories a binary built elsewhere needs.
    fn reachable() -> Vec<PathBuf> {
        let store = PathBuf::from("/nix/store");
        assert!(
            store.is_dir(),
            "this workspace builds against a nix store, and the slice runs a binary out of it",
        );

        [
            store,
            PathBuf::from("/bin"),
            PathBuf::from("/usr"),
            PathBuf::from("/lib"),
            PathBuf::from("/lib64"),
        ]
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
    }

    /// The shared prefix every invocation carries.
    fn base(
        scratch: &Scratch,
        allow_tool: bool,
    ) -> Vec<String> {
        let mut args = vec![
            "--data-dir".to_owned(),
            scratch.data().display().to_string(),
            "--confiner".to_owned(),
            confiner().display().to_string(),
        ];
        if allow_tool {
            args.push("--allow-tool".to_owned());
            args.push(shell().display().to_string());
        }
        for path in reachable() {
            args.push("--allow-path".to_owned());
            args.push(path.display().to_string());
        }

        args
    }

    fn run(args: &[String]) -> Ran {
        let output = Command::new(binary())
            .args(args)
            .output()
            .expect("the binary runs");

        Ran {
            ok: output.status.success(),
            code: output.status.code(),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn status(scratch: &Scratch) -> Ran {
        let mut args = base(scratch, true);
        args.extend([
            "status".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
        ]);

        run(&args)
    }

    fn reconcile(scratch: &Scratch) -> Ran {
        let mut args = base(scratch, true);
        args.extend([
            "reconcile".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
        ]);

        run(&args)
    }

    /// One `submit` that writes `hello` into `out.txt` in the workload's own workspace.
    fn submit(
        scratch: &Scratch,
        key: &str,
        extra: &[&str],
    ) -> Ran {
        let mut args = base(scratch, true);
        args.extend([
            "submit".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--idempotency-key".to_owned(),
            key.to_owned(),
            "--program".to_owned(),
            shell().display().to_string(),
            "--arg".to_owned(),
            "-c".to_owned(),
            "--arg".to_owned(),
            "printf hello > out.txt".to_owned(),
            "--artifact".to_owned(),
            "out.txt".to_owned(),
            "--deadline-seconds".to_owned(),
            "30".to_owned(),
        ]);
        args.extend(extra.iter().map(|value| (*value).to_owned()));

        run(&args)
    }

    #[test]
    fn a_submit_runs_one_confined_tool_and_records_what_it_produced() {
        let scratch = Scratch::new("happy");

        let submitted = submit(&scratch, KEY_A, &[]);

        assert!(submitted.ok, "{}\n{}", submitted.out, submitted.err);
        assert_eq!(submitted.value("replayed").as_deref(), Some("false"));
        assert_eq!(submitted.value("phase").as_deref(), Some("Accepting"));
        assert_eq!(
            submitted.value("outcome").as_deref(),
            Some("Some(Completed)"),
        );
        assert!(
            submitted
                .require("execution=")
                .ends_with("resolution=Some(Finished { code: 0 })"),
            "{}",
            submitted.out,
        );
        // The workload redirected everything into the artifact, so standard output held nothing, and
        // the artifact holds the five bytes of `hello`.
        assert_eq!(submitted.value("stdout").as_deref(), Some(EMPTY_SHA256),);
        assert!(
            submitted
                .out
                .contains(&format!("artifact=out.txt hash={HELLO_SHA256} bytes=5")),
            "{}",
            submitted.out,
        );
        assert_eq!(scratch.started(), 0, "a finished run is cleaned up");

        let report = status(&scratch);

        assert!(report.ok, "{}\n{}", report.out, report.err);
        assert_eq!(report.value("phase").as_deref(), Some("Accepting"));
        assert_eq!(report.value("outcome").as_deref(), Some("Some(Completed)"));
        // The status a finished run leaves is the hand-off, not the settlement: the result was
        // applied to the session exactly once.
        assert!(
            report
                .require("execution=")
                .ends_with("state=Applied { code: 0 }"),
            "{}",
            report.out,
        );
        assert!(report.out.contains(HELLO_SHA256), "{}", report.out);
    }

    #[test]
    fn a_retried_command_replays_the_recorded_outcome_without_running_again() {
        let scratch = Scratch::new("replay");

        let first = submit(&scratch, KEY_A, &[]);
        assert!(first.ok, "{}\n{}", first.out, first.err);
        let second = submit(&scratch, KEY_A, &[]);

        assert!(second.ok, "{}\n{}", second.out, second.err);
        assert_eq!(second.value("replayed").as_deref(), Some("true"));
        assert_eq!(
            second.value("execution"),
            first.value("execution"),
            "the second submit reports the first run's execution",
        );
        assert_eq!(
            second.phrase("artifact=out.txt"),
            first.phrase("artifact=out.txt"),
            "and the first run's artifact",
        );

        let report = status(&scratch);
        assert_eq!(
            report
                .out
                .lines()
                .filter(|line| line.starts_with("execution="))
                .count(),
            1,
            "a retried command did not launch a second execution:\n{}",
            report.out,
        );
        assert_eq!(
            report
                .out
                .lines()
                .filter(|line| line.starts_with("turn="))
                .count(),
            1,
            "{}",
            report.out,
        );
    }

    #[test]
    fn a_tool_the_authority_never_named_is_refused_before_anything_is_created() {
        let scratch = Scratch::new("denied");

        let mut args = base(&scratch, false);
        args.extend([
            "submit".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--idempotency-key".to_owned(),
            KEY_A.to_owned(),
            "--program".to_owned(),
            shell().display().to_string(),
            "--arg".to_owned(),
            "-c".to_owned(),
            "--arg".to_owned(),
            "printf hello".to_owned(),
        ]);
        let refused = run(&args);

        assert!(refused.ok, "{}\n{}", refused.out, refused.err);
        assert!(
            refused
                .require("execution=")
                .ends_with("resolution=Some(Refused)"),
            "{}",
            refused.out,
        );
        assert_eq!(refused.value("outcome").as_deref(), Some("Some(Failed)"));
        assert_eq!(scratch.started(), 0, "a refused workload never started");
        assert!(
            !refused.out.contains("artifact="),
            "a refused run produced nothing:\n{}",
            refused.out,
        );
    }

    #[test]
    fn a_crash_between_the_intent_and_the_outcome_leaves_an_unknown_outcome() {
        let scratch = Scratch::new("crash");

        let early = submit(
            &scratch,
            KEY_CRASH_EARLY,
            &["--crash-after", "before-dispatch"],
        );
        assert_eq!(early.code, Some(97), "{}\n{}", early.out, early.err);
        assert_eq!(
            scratch.started(),
            0,
            "the intent was committed before anything was created",
        );
        let after_early = status(&scratch);
        assert_eq!(
            after_early
                .out
                .lines()
                .filter(|line| line.contains("state=InFlight"))
                .count(),
            1,
            "{}",
            after_early.out,
        );

        let late = submit(
            &scratch,
            KEY_CRASH_LATE,
            &["--crash-after", "after-dispatch"],
        );
        assert_eq!(late.code, Some(97), "{}\n{}", late.out, late.err);
        assert_eq!(
            scratch.started(),
            1,
            "the workload ran and the supervisor's directory outlived the crash",
        );

        let after_late = status(&scratch);
        assert!(
            after_late.out.contains("state=Settled(Unknown)"),
            "the second submit's recovery recorded what it cannot prove:\n{}",
            after_late.out,
        );
        assert_eq!(
            after_late
                .out
                .lines()
                .filter(|line| line.contains("state=InFlight"))
                .count(),
            1,
            "{}",
            after_late.out,
        );
        assert!(
            !after_late.out.contains("result execution="),
            "no run recorded a result:\n{}",
            after_late.out,
        );

        let recovered = reconcile(&scratch);

        assert!(recovered.ok, "{}\n{}", recovered.out, recovered.err);
        assert_eq!(
            recovered
                .out
                .lines()
                .filter(|line| line.starts_with("recovered execution="))
                .count(),
            1,
            "{}",
            recovered.out,
        );

        let after = status(&scratch);
        assert_eq!(
            after
                .out
                .lines()
                .filter(|line| line.contains("state=Settled(Unknown)"))
                .count(),
            2,
            "recovery records what it cannot prove:\n{}",
            after.out,
        );

        // A second reconcile finds nothing left in flight, so it writes nothing.
        let again = reconcile(&scratch);
        assert!(again.ok, "{}\n{}", again.out, again.err);
        assert_eq!(
            again
                .out
                .lines()
                .filter(|line| line.starts_with("recovered execution="))
                .count(),
            0,
            "{}",
            again.out,
        );
    }

    /// The serving kernel, stopped when the test drops it.
    struct Serving(Child);

    impl Drop for Serving {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[tokio::test]
    async fn the_relay_hands_committed_notices_to_a_subscriber() {
        let scratch = Scratch::new("relay");
        let socket = scratch.0.join("bus.sock");
        let _serving = Serving(
            Command::new(binary())
                .args(base(&scratch, true))
                .args([
                    "serve".to_owned(),
                    "--socket".to_owned(),
                    socket.display().to_string(),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("the serving kernel starts"),
        );

        for _ in 0..200 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(socket.exists(), "the bus bound its socket");

        let first = submit(&scratch, KEY_A, &[]);
        assert!(first.ok, "{}\n{}", first.out, first.err);
        let second = submit(&scratch, KEY_B, &[]);
        assert!(second.ok, "{}\n{}", second.out, second.err);

        let session = SESSION
            .parse::<uuid::Uuid>()
            .map(mokmok_protocol::SessionID::from_uuid)
            .expect("the session is a uuid");
        let mut subscriber =
            Subscriber::open(&socket, Topic::Session(session), SubscriberId::new())
                .await
                .expect("a subscriber connects");

        let mut notices = Vec::new();
        for _ in 0..20 {
            // A subscriber that has caught up parks until the next commit lands, and this test will
            // not wait for one that never comes.
            let Ok(Ok(batch)) =
                tokio::time::timeout(std::time::Duration::from_secs(2), subscriber.next()).await
            else {
                break;
            };
            notices.extend(
                batch
                    .events()
                    .iter()
                    .map(|event| event.payload().as_str().to_owned())
                    .collect::<Vec<_>>(),
            );
            if notices.len() >= 4 {
                break;
            }
        }

        assert!(
            notices.len() >= 4,
            "two submits wrote four notices, and the bus holds what it was given: {notices:#?}",
        );
        assert!(
            notices.iter().all(|notice| notice.contains(SESSION)),
            "every notice names the session it belongs to: {notices:#?}",
        );
        assert!(
            notices.iter().any(|notice| notice.contains("finished")),
            "the settlement notice names the outcome: {notices:#?}",
        );
    }

    /// The binary's own directory holds the confiner the sandbox supervises with.
    #[test]
    fn the_confiner_sits_beside_the_binary() {
        assert!(confiner().is_file(), "{}", confiner().display());
        assert!(Path::new("/nix/store").is_dir());
    }
}
