//! The agent loop, driven the way a caller drives it: through `mokmokd`, with a scripted provider
//! and a real confined tool, against a real SQLite log and artifact store.
//!
//! No test here reaches the network: the scripted provider's answers come from a file, so the whole
//! loop — model call, artifact store, authority decision, confined tool, settlement, close — runs in
//! the gate, which has none.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use clap::Parser as _;
    use mokmok_agent_runtime::{Action, Progress, Record, UNKNOWN_WARNING, turn_id};
    use mokmok_artifact::{BlobStore, ContentHash};
    use mokmok_kernel::agent::Driver;
    use mokmok_kernel::{Config, Db, Run, uuid_of};
    use mokmok_protocol::{Moment, SessionID};
    use mokmok_state_machine::{Phase, TurnOutcome};
    use serde_json::json;

    const SESSION: &str = "018f2f1e-0000-7000-8000-000000000000";

    /// A directory of this test's own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mokmok-agent-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            // The checkout has to exist before it can be granted with `--allow-write`.
            std::fs::create_dir_all(path.join("checkout")).expect("the scratch checkout is made");

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
    }

    fn binary() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_mokmokd"))
    }

    fn confiner() -> PathBuf {
        let path = binary()
            .parent()
            .expect("the binary has a directory")
            .join("mokmok-sandbox-confine");
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

        panic!("no shell on PATH, so the agent cannot run a tool");
    }

    /// The directories the workload may read and execute in.
    fn reachable() -> Vec<PathBuf> {
        let store = PathBuf::from("/nix/store");
        assert!(
            store.is_dir(),
            "this workspace builds against a nix store, and the agent runs a binary out of it",
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
    fn base(scratch: &Scratch) -> Vec<String> {
        let mut args = vec![
            "--data-dir".to_owned(),
            scratch.data().display().to_string(),
            "--confiner".to_owned(),
            confiner().display().to_string(),
            "--allow-tool".to_owned(),
            shell().display().to_string(),
            "--allow-write".to_owned(),
            scratch.checkout().display().to_string(),
        ];
        for path in reachable() {
            args.push("--allow-path".to_owned());
            args.push(path.display().to_string());
        }

        args
    }

    /// The shared prefix with no read or execute grant: the granted program is named, and nothing
    /// it must load to start is inside what the workload may reach.
    fn without_path_grants(scratch: &Scratch) -> Vec<String> {
        vec![
            "--data-dir".to_owned(),
            scratch.data().display().to_string(),
            "--confiner".to_owned(),
            confiner().display().to_string(),
            "--allow-tool".to_owned(),
            shell().display().to_string(),
            "--allow-write".to_owned(),
            scratch.checkout().display().to_string(),
        ]
    }

    fn ran(output: &Output) -> Ran {
        Ran {
            ok: output.status.success(),
            code: output.status.code(),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Runs the binary with the crash knob unlocked, as the crash harness does.
    fn run(args: &[String]) -> Ran {
        ran(&Command::new(binary())
            .env("MOKMOKD_CRASH", "1")
            .args(args)
            .output()
            .expect("the binary runs"))
    }

    /// Writes the scripted provider's answers to `path`.
    fn script(
        path: &Path,
        answers: &[serde_json::Value],
    ) {
        std::fs::write(
            path,
            serde_json::to_vec(answers).expect("the script encodes"),
        )
        .expect("the script is written");
    }

    /// One `agent run` for the scratch session.
    fn agent_args(
        scratch: &Scratch,
        script_path: &Path,
        extra: &[&str],
    ) -> Vec<String> {
        let mut args = base(scratch);
        args.extend([
            "agent".to_owned(),
            "run".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--goal".to_owned(),
            "write hello into out.txt".to_owned(),
            "--checkout".to_owned(),
            scratch.checkout().display().to_string(),
            "--script".to_owned(),
            script_path.display().to_string(),
            "--max-turns".to_owned(),
            "6".to_owned(),
        ]);
        args.extend(extra.iter().map(|value| (*value).to_owned()));

        args
    }

    fn status(scratch: &Scratch) -> Ran {
        let mut args = base(scratch);
        args.extend([
            "status".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
        ]);

        run(&args)
    }

    #[test]
    fn the_whole_loop_runs_a_command_and_closes_the_session() {
        let scratch = Scratch::new("loop");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({
                    "action": "command",
                    "command": format!("cd {} && printf hello > out.txt", scratch.checkout().display()),
                }),
                json!({"action": "final"}),
            ],
        );

        let ran = run(&agent_args(&scratch, &path, &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(
            std::fs::read_to_string(scratch.checkout().join("out.txt")).expect("the command wrote"),
            "hello",
        );

        let report = status(&scratch);
        assert!(report.ok, "{}\n{}", report.out, report.err);
        assert_eq!(report.value("phase").as_deref(), Some("Closed(Completed)"));
        for head in ["prompt ", "answer ", "call ", "result "] {
            assert!(
                report.phrase(head).is_some(),
                "status shows the {head}transcript:\n{}",
                report.out,
            );
        }
    }

    #[test]
    fn a_crash_before_dispatch_resumes_without_running_the_command() {
        let scratch = Scratch::new("crash");
        let path = scratch.0.join("script.json");
        let command = format!(
            "cd {} && printf ran > marker.txt",
            scratch.checkout().display(),
        );
        script(&path, &[json!({"action": "command", "command": command})]);

        let crashed = run(&agent_args(
            &scratch,
            &path,
            &["--crash-after", "before-dispatch", "--steps", "3"],
        ));
        assert_eq!(crashed.code, Some(97), "{}\n{}", crashed.out, crashed.err);
        assert!(
            !scratch.checkout().join("marker.txt").exists(),
            "the crash stopped before anything ran:\n{}",
            crashed.out,
        );

        // The resumed model answers final. An automatic re-dispatch of the in-flight command would
        // leave the marker behind, so its absence is the measurement.
        script(&path, &[json!({"action": "final"})]);
        let resumed = run(&agent_args(&scratch, &path, &[]));
        assert!(resumed.ok, "{}\n{}", resumed.out, resumed.err);
        assert!(
            !scratch.checkout().join("marker.txt").exists(),
            "the resume never re-dispatched the command:\n{}",
            resumed.out,
        );

        let report = status(&scratch);
        let prompt = report
            .out
            .lines()
            .rfind(|line| line.starts_with("prompt ") && line.contains(" number=2 "))
            .unwrap_or_else(|| panic!("no prompt for turn 2 in:\n{}", report.out));
        let hash = prompt
            .split_whitespace()
            .find_map(|field| field.strip_prefix("hash="))
            .expect("the prompt names its context bytes");
        let store = BlobStore::open(scratch.data().join("artifacts")).expect("the store opens");
        let context = store
            .read(hash.parse::<ContentHash>().expect("the hash is hex"))
            .expect("the context bytes are stored");
        let context = String::from_utf8(context).expect("the context is JSON text");

        assert!(
            context.contains(UNKNOWN_WARNING),
            "the next prompt carries the unproven-outcome warning:\n{context}",
        );
        assert!(
            context.contains("marker.txt"),
            "and still shows the command whose outcome is unknown:\n{context}",
        );
    }

    #[test]
    fn a_cancelled_session_closes_cancelled() {
        let scratch = Scratch::new("cancel");
        let path = scratch.0.join("script.json");
        script(&path, &[]);

        // One step opens the session and writes the task, and the model is never asked.
        let opened = run(&agent_args(&scratch, &path, &["--steps", "1"]));
        assert!(opened.ok, "{}\n{}", opened.out, opened.err);
        assert_eq!(
            status(&scratch).value("phase").as_deref(),
            Some("Accepting")
        );

        let mut cancel = base(&scratch);
        cancel.extend([
            "agent".to_owned(),
            "cancel".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
        ]);
        let cancelled = run(&cancel);
        assert!(cancelled.ok, "{}\n{}", cancelled.out, cancelled.err);

        let closed = run(&agent_args(&scratch, &path, &[]));
        assert!(closed.ok, "{}\n{}", closed.out, closed.err);
        assert_eq!(
            status(&scratch).value("phase").as_deref(),
            Some("Closed(Cancelled)"),
        );
    }

    #[test]
    fn a_command_no_grant_reaches_is_refused_rather_than_run() {
        let scratch = Scratch::new("refused");
        let path = scratch.0.join("script.json");
        let command = format!(
            "cd {} && printf ran > marker.txt",
            scratch.checkout().display(),
        );
        script(
            &path,
            &[
                json!({"action": "command", "command": command}),
                json!({"action": "final"}),
            ],
        );

        let mut args = without_path_grants(&scratch);
        args.extend([
            "agent".to_owned(),
            "run".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--goal".to_owned(),
            "write marker.txt".to_owned(),
            "--checkout".to_owned(),
            scratch.checkout().display().to_string(),
            "--script".to_owned(),
            path.display().to_string(),
            "--max-turns".to_owned(),
            "4".to_owned(),
        ]);

        let ran = run(&args);
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert!(
            !scratch.checkout().join("marker.txt").exists(),
            "a refused command must not have run:\n{}",
            ran.out,
        );

        let report = status(&scratch);
        assert!(report.ok, "{}\n{}", report.out, report.err);
        let result = report
            .phrase("result ")
            .unwrap_or_else(|| panic!("no result line:\n{}", report.out));
        assert!(result.contains("exit=Refused"), "{result}");
    }

    #[test]
    fn a_scripted_final_answer_records_its_prose() {
        let scratch = Scratch::new("prose");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({
                    "action": "command",
                    "command": format!("cd {} && printf hello > out.txt", scratch.checkout().display()),
                }),
                json!({"action": "final", "say": "the file is written"}),
            ],
        );

        let ran = run(&agent_args(&scratch, &path, &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);

        let db = Db::open(&scratch.data().join("events.sqlite")).expect("the log opens");
        let session =
            SessionID::from_uuid(uuid_of(SESSION, "session").expect("the session is a uuid"));
        let log = db.log(session).expect("the log reads");
        let prose = log.ordered.iter().find_map(|record| match record {
            Record::Answer(answer) if answer.action == Action::Final => Some(answer.prose.as_str()),
            _ => None,
        });

        assert_eq!(prose, Some("the file is written"));

        // The same run, read back the way a human reads a conversation: `status` is the only surface
        // that shows the agent's words outside the TUI, so the rendering is asserted and not assumed.
        let read_back = status(&scratch);
        assert!(read_back.ok, "{}\n{}", read_back.out, read_back.err);
        let final_answer = read_back
            .out
            .lines()
            .find(|line| line.starts_with("answer ") && line.contains("action=Final"))
            .expect("the final answer is reported");
        assert!(
            final_answer.contains("say=\"the file is written\""),
            "the agent's words are reported: {final_answer}",
        );
    }

    /// The validated configuration and run one scripted `agent run` invocation names.
    fn driver_config(
        scratch: &Scratch,
        script_path: &Path,
    ) -> (Config, Run) {
        let mut args = vec!["mokmokd".to_owned()];
        args.extend(base(scratch));
        args.extend([
            "agent".to_owned(),
            "run".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--goal".to_owned(),
            "hold a conversation".to_owned(),
            "--checkout".to_owned(),
            scratch.checkout().display().to_string(),
            "--script".to_owned(),
            script_path.display().to_string(),
        ]);
        let parsed = mokmok_kernel::Args::try_parse_from(&args).expect("the command line parses");
        let config = Config::resolve(&parsed).expect("the configuration resolves");
        let mokmok_kernel::Command::Agent(mokmok_kernel::Agent::Run(run)) = parsed.command else {
            panic!("the command line named an agent run");
        };

        (config, run)
    }

    #[tokio::test]
    async fn a_driver_speaks_then_waits_and_extends_the_session() {
        let scratch = Scratch::new("driver");
        let path = scratch.0.join("script.json");
        script(&path, &[json!({"action": "final", "say": "hi there"})]);
        let (config, run) = driver_config(&scratch, &path);
        let at = Moment::from_millis(0);
        let mut driver = Driver::open(&config, &run, at).expect("the driver opens");
        let session = driver.session();

        driver.speak("hello").expect("the line opens turn 1");

        let awaiting = loop {
            match driver.step().await.expect("the step runs") {
                Progress::Awaiting => break true,
                Progress::Terminal(_) => break false,
                Progress::Yielded | Progress::Advanced(_) => {},
            }
        };
        assert!(awaiting, "the driver waits for the human's next line");

        let transcript = driver.transcript().expect("the fold reads");
        assert_eq!(transcript.session.phase(), Phase::Accepting);
        let (turn, state) = transcript.newest_turn().expect("a newest turn");
        assert_eq!(turn, turn_id(session, 1));
        assert_eq!(state.outcome(), Some(TurnOutcome::Completed));
        assert_eq!(
            transcript.newest_answer(turn).expect("an answer").prose,
            "hi there",
        );

        driver
            .speak("again")
            .expect("the line extends the conversation");
        let extended = driver.transcript().expect("the fold reads");
        assert_eq!(
            extended.newest_turn().expect("a newest turn").0,
            turn_id(session, 2),
            "the second line opened a second turn rather than a new session",
        );
        assert_eq!(extended.speech(turn_id(session, 2)), Some("again"));
    }

    #[tokio::test]
    async fn the_attempt_ceiling_waits_when_a_line_is_held() {
        let scratch = Scratch::new("driver-ceiling");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "malformed", "reason": "one"}),
                json!({"action": "malformed", "reason": "two"}),
                json!({"action": "malformed", "reason": "three"}),
            ],
        );
        let (config, run) = driver_config(&scratch, &path);
        let mut driver =
            Driver::open(&config, &run, Moment::from_millis(0)).expect("the driver opens");

        driver.speak("hello").expect("the line opens turn 1");

        let awaiting = loop {
            match driver.step().await.expect("the step runs") {
                Progress::Awaiting => break true,
                Progress::Terminal(_) => break false,
                Progress::Yielded | Progress::Advanced(_) => {},
            }
        };
        assert!(awaiting, "a spent ceiling waits rather than closing");

        let transcript = driver.transcript().expect("the fold reads");
        assert_eq!(transcript.session.phase(), Phase::Accepting);
        assert_eq!(
            transcript.newest_turn().expect("a newest turn").1.outcome(),
            Some(TurnOutcome::Failed),
        );
    }

    #[test]
    fn a_spent_ceiling_closes_a_goal_run() {
        let scratch = Scratch::new("ceiling-goal-run");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "malformed", "reason": "one"}),
                json!({"action": "malformed", "reason": "two"}),
                json!({"action": "malformed", "reason": "three"}),
            ],
        );

        let ran = run(&agent_args(&scratch, &path, &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(ran.value("halt").as_deref(), Some("Attempts"));
        assert_eq!(
            status(&scratch).value("phase").as_deref(),
            Some("Closed(Completed)"),
        );
    }
}
