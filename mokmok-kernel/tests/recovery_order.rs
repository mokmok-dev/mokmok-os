//! The order a daemon starts its services in, driven the way a peer meets it: connect the moment
//! the socket exists, and the log must already be recovered.
//!
//! `serve` binds the bus and runs required recovery, in that order, so a peer that submits as soon
//! as it can connect can race the pass that is unwinding a crashed session. Both writers settle the
//! same execution, and the stream then holds two settlements of one execution, which is a history
//! no reader can replay. This test seeds crashed sessions, starts the daemon, and asserts the first
//! observable moment a peer can act on is already past recovery.
//!
//! The harness lives inside a `#[cfg(test)]` module because that is what tells the workspace's
//! clippy settings that this is test code, where `expect` and `panic!` are allowed.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    /// How many sessions a crash is left in. Recovery appends a settlement per session, so the
    /// window this test looks for grows with the count.
    const SEEDED: u32 = 24;

    /// A directory of this test's own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "mokmok-recovery-order-{name}-{}",
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

    /// One finished invocation of the binary.
    struct Ran {
        code: Option<i32>,
        out: String,
        err: String,
    }

    /// The daemon this test started, stopped when the test ends.
    struct Serving(Child);

    impl Drop for Serving {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn binary() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_mokmokd"))
    }

    /// A `cargo mutants` run names the confiner with `MOKMOK_CONFINER`.
    fn confiner() -> PathBuf {
        if let Some(named) = std::env::var_os("MOKMOK_CONFINER") {
            let path = PathBuf::from(named);
            assert!(path.is_file(), "{} is not a file", path.display());

            return path;
        }

        let path = binary()
            .parent()
            .expect("the binary has a directory")
            .join("mokmok-sandbox-confine");
        assert!(
            path.is_file(),
            "{} is missing: build the workspace (cargo build --workspace) before running this \
             package's tests alone, or name the confiner with MOKMOK_CONFINER",
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

    /// The directories the workload may read and execute in.
    fn reachable() -> Vec<PathBuf> {
        let store = PathBuf::from("/nix/store");
        assert!(store.is_dir(), "this workspace builds against a nix store");

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

    fn base(scratch: &Scratch) -> Vec<String> {
        let mut args = vec![
            "--data-dir".to_owned(),
            scratch.data().display().to_string(),
            "--confiner".to_owned(),
            confiner().display().to_string(),
            "--allow-tool".to_owned(),
            shell().display().to_string(),
        ];
        for path in reachable() {
            args.push("--allow-path".to_owned());
            args.push(path.display().to_string());
        }

        args
    }

    fn run(
        scratch: &Scratch,
        args: &[String],
    ) -> Ran {
        let output = Command::new(binary())
            .env("MOKMOKD_CRASH", "1")
            .env("XDG_CONFIG_HOME", scratch.0.join("config"))
            .args(args)
            .output()
            .expect("the binary runs");

        Ran {
            code: output.status.code(),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn session(index: u32) -> String {
        format!("018f2f1e-0000-7000-8000-{index:012x}")
    }

    fn key(index: u32) -> String {
        format!("018f2f1e-0000-7000-8001-{index:012x}")
    }

    /// One crashed invocation whose session is left holding an execution with no outcome.
    fn crash(
        scratch: &Scratch,
        index: u32,
    ) {
        let mut args = base(scratch);
        args.extend([
            "submit".to_owned(),
            "--session".to_owned(),
            session(index),
            "--idempotency-key".to_owned(),
            key(index),
            "--program".to_owned(),
            shell().display().to_string(),
            "--arg".to_owned(),
            "-c".to_owned(),
            "--arg".to_owned(),
            "printf hello".to_owned(),
            "--crash-after".to_owned(),
            "before-dispatch".to_owned(),
        ]);

        let ran = run(&scratch, &args);
        assert_eq!(ran.code, Some(97), "{}\n{}", ran.out, ran.err);
    }

    /// How many executions one session reports as still in flight.
    fn in_flight(
        scratch: &Scratch,
        index: u32,
    ) -> usize {
        let mut args = base(scratch);
        args.extend(["status".to_owned(), "--session".to_owned(), session(index)]);

        let ran = run(&scratch, &args);
        assert!(
            ran.code == Some(0),
            "status failed for {}: {}\n{}",
            session(index),
            ran.out,
            ran.err,
        );

        ran.out
            .lines()
            .filter(|line| line.contains("state=InFlight"))
            .count()
    }

    fn wait_for_socket(socket: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if socket.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        panic!("{} never appeared", socket.display());
    }

    #[test]
    fn the_bus_accepts_nobody_before_recovery_has_run() {
        let scratch = Scratch::new("serve");
        let socket = scratch.0.join("bus.sock");

        for index in 0..SEEDED {
            crash(&scratch, index);
        }

        // The control: without it the assertion below would pass against a log that was never
        // crashed, and the run would prove nothing.
        assert_eq!(
            in_flight(&scratch, 0),
            1,
            "the crash left nothing to recover, so this test cannot see the ordering it checks",
        );

        let _serving = Serving(
            Command::new(binary())
                .env("XDG_CONFIG_HOME", scratch.0.join("config"))
                .args(base(&scratch))
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
        wait_for_socket(&socket);

        let unsettled = (0..SEEDED)
            .filter(|index| in_flight(&scratch, *index) > 0)
            .count();
        assert_eq!(
            unsettled, 0,
            "a peer that connected when the socket appeared found {unsettled} sessions a crash left \
             in flight, so recovery had not run yet",
        );
    }
}
