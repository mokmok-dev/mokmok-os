//! The agent loop, driven the way a caller drives it: through `mokmokd`, with a scripted provider
//! and a real confined tool, against a real SQLite log and artifact store.
//!
//! No test here reaches the network: the scripted provider's answers come from a file, so the whole
//! loop — model call, artifact store, authority decision, confined tool, settlement, close — runs in
//! the gate, which has none.

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use clap::Parser as _;
    use mokmok_agent_runtime::{
        Action, EditOutcome, NO_SEARCH_ROOT, Progress, Record, UNKNOWN_WARNING, speak, turn_id,
    };
    use mokmok_artifact::{BlobStore, ContentHash};
    use mokmok_kernel::agent::{Driver, Ledger};
    use mokmok_kernel::{Config, Db, ProcessEnvironment, Run, uuid_of};
    use mokmok_protocol::{Moment, SessionID};
    use mokmok_state_machine::{Phase, TurnOutcome};
    use serde_json::json;

    const SESSION: &str = "018f2f1e-0000-7000-8000-000000000000";
    /// A second session, for a run whose step count is read before the run that asserts on it.
    const COUNTED: &str = "018f2f1e-0000-7000-8000-0000000000e5";
    /// The idempotency identity one `submit` in this file runs under.
    const KEY: &str = "018f2f1e-0000-7000-8000-0000000000f1";

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

    /// The child every run here spawns: the crash knob unlocked and this test's configuration root,
    /// both set the way the crash harness sets them. A case that pins a variable of its own, or
    /// removes one, starts from here.
    fn child(scratch: &Scratch) -> Command {
        let mut command = Command::new(binary());
        command
            .env("MOKMOKD_CRASH", "1")
            .env("XDG_CONFIG_HOME", scratch.0.join("config"));

        command
    }

    /// Runs the binary with the crash knob unlocked, as the crash harness does.
    fn run(
        scratch: &Scratch,
        args: &[String],
    ) -> Ran {
        ran(&child(scratch).args(args).output().expect("the binary runs"))
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
        agent_args_of(SESSION, scratch, script_path, extra)
    }

    /// One `agent run` for `session`.
    fn agent_args_of(
        session: &str,
        scratch: &Scratch,
        script_path: &Path,
        extra: &[&str],
    ) -> Vec<String> {
        let mut args = base(scratch);
        args.extend([
            "agent".to_owned(),
            "run".to_owned(),
            "--session".to_owned(),
            session.to_owned(),
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

        run(scratch, &args)
    }

    /// A stub endpoint on this host that answers one chat-completions response, and the
    /// `Authorization` header it received.
    ///
    /// Cleartext HTTP is allowed for loopback, so this reaches the provider's credential without
    /// the network the gate has none of.
    fn stub_endpoint() -> (u16, Arc<Mutex<Option<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("the bound address").port();
        let seen = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&seen);
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&buffer[..read]),
                }
            }
            let request = String::from_utf8_lossy(&request).into_owned();
            *captured.lock().unwrap() = request.lines().find_map(|line| {
                let lower = line.to_ascii_lowercase();

                lower
                    .strip_prefix("authorization: ")
                    .map(|_| line["authorization: ".len()..].to_owned())
            });

            let body = br#"{"choices":[{"message":{"content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                String::from_utf8_lossy(body),
            );
            let _ = stream.write_all(response.as_bytes());
        });

        (port, seen)
    }

    fn reconcile(scratch: &Scratch) -> Ran {
        let mut args = base(scratch);
        args.extend([
            "reconcile".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
        ]);

        run(scratch, &args)
    }

    /// One `submit` of `body` through the operator's shell, which is the path the kernel takes
    /// before the agent's own loop is ever opened.
    fn submit_args(
        scratch: &Scratch,
        body: &str,
    ) -> Vec<String> {
        let mut args = base(scratch);
        args.extend([
            "submit".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--idempotency-key".to_owned(),
            KEY.to_owned(),
            "--program".to_owned(),
            shell().display().to_string(),
            "--arg".to_owned(),
            "-c".to_owned(),
            "--arg".to_owned(),
            body.to_owned(),
            "--artifact".to_owned(),
            "out.txt".to_owned(),
            "--deadline-seconds".to_owned(),
            "30".to_owned(),
        ]);

        args
    }

    fn prompt_context(
        scratch: &Scratch,
        number: u64,
    ) -> String {
        let report = status(scratch);
        assert!(report.ok, "{}\n{}", report.out, report.err);
        let head = format!(" number={number} ");
        let prompt = report
            .out
            .lines()
            .rfind(|line| line.starts_with("prompt ") && line.contains(&head))
            .unwrap_or_else(|| panic!("no prompt for turn {number} in:\n{}", report.out));
        let hash = prompt
            .split_whitespace()
            .find_map(|field| field.strip_prefix("hash="))
            .expect("the prompt names its context bytes");
        let store = BlobStore::open(scratch.data().join("artifacts")).expect("the store opens");
        let context = store
            .read(hash.parse::<ContentHash>().expect("the hash is hex"))
            .expect("the context bytes are stored");

        String::from_utf8(context).expect("the context is JSON text")
    }

    fn log_of(scratch: &Scratch) -> (mokmok_kernel::Log, SessionID) {
        let db = Db::open(&scratch.data().join("events.sqlite")).expect("the log opens");
        let session =
            SessionID::from_uuid(uuid_of(SESSION, "session").expect("the session is a uuid"));

        (db.log(session).expect("the log reads"), session)
    }

    fn find_of(log: &mokmok_kernel::Log) -> &mokmok_agent_runtime::FindRecord {
        log.ordered
            .iter()
            .find_map(|record| match record {
                Record::Find(find) => Some(find),
                _ => None,
            })
            .expect("the session holds one find record")
    }

    fn edit_intent_of(log: &mokmok_kernel::Log) -> &mokmok_agent_runtime::EditIntentRecord {
        log.ordered
            .iter()
            .find_map(|record| match record {
                Record::EditIntent(edit) => Some(edit),
                _ => None,
            })
            .expect("the session holds one edit intent record")
    }

    fn edit_outcome_of(log: &mokmok_kernel::Log) -> &mokmok_agent_runtime::EditOutcomeRecord {
        log.ordered
            .iter()
            .find_map(|record| match record {
                Record::EditOutcome(edit) => Some(edit),
                _ => None,
            })
            .expect("the session holds one edit outcome record")
    }

    fn subagent_intent_of(log: &mokmok_kernel::Log) -> &mokmok_agent_runtime::SubagentIntentRecord {
        log.ordered
            .iter()
            .find_map(|record| match record {
                Record::SubagentIntent(subagent) => Some(subagent),
                _ => None,
            })
            .expect("the session holds one subagent intent record")
    }

    fn subagent_outcome_of(
        log: &mokmok_kernel::Log
    ) -> &mokmok_agent_runtime::SubagentOutcomeRecord {
        log.ordered
            .iter()
            .find_map(|record| match record {
                Record::SubagentOutcome(subagent) => Some(subagent),
                _ => None,
            })
            .expect("the session holds one subagent outcome record")
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

        let ran = run(&scratch, &agent_args(&scratch, &path, &[]));
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
    fn a_run_takes_the_provider_group_from_the_configuration_file() {
        let scratch = Scratch::new("config-file");
        let directory = scratch.0.join("config/mokmok");
        std::fs::create_dir_all(&directory).expect("the config directory is made");
        script(
            &directory.join("answers.json"),
            &[
                json!({
                    "action": "command",
                    "command": format!("cd {} && printf hello > out.txt", scratch.checkout().display()),
                }),
                json!({"action": "final"}),
            ],
        );
        std::fs::write(
            directory.join("config.toml"),
            "provider = \"scripted\"\nscript = \"answers.json\"\n",
        )
        .expect("the config file is written");

        let mut args = base(&scratch);
        args.extend([
            "agent".to_owned(),
            "run".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--goal".to_owned(),
            "write hello into out.txt".to_owned(),
            "--checkout".to_owned(),
            scratch.checkout().display().to_string(),
            "--max-turns".to_owned(),
            "6".to_owned(),
        ]);

        let ran = run(&scratch, &args);

        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(
            std::fs::read_to_string(scratch.checkout().join("out.txt")).expect("the command wrote"),
            "hello",
        );
    }

    #[test]
    fn a_run_reads_the_script_the_environment_names() {
        let scratch = Scratch::new("config-variable");
        let directory = scratch.0.join("config/mokmok");
        std::fs::create_dir_all(&directory).expect("the config directory is made");
        script(
            &directory.join("answers.json"),
            &[
                json!({
                    "action": "command",
                    "command": format!("cd {} && printf hello > out.txt", scratch.checkout().display()),
                }),
                json!({"action": "final"}),
            ],
        );
        std::fs::write(
            directory.join("config.toml"),
            "provider = \"scripted\"\nscript = \"${MOKMOK_SCRIPT}\"\n",
        )
        .expect("the config file is written");

        let mut args = base(&scratch);
        args.extend([
            "agent".to_owned(),
            "run".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--goal".to_owned(),
            "write hello into out.txt".to_owned(),
            "--checkout".to_owned(),
            scratch.checkout().display().to_string(),
            "--max-turns".to_owned(),
            "6".to_owned(),
        ]);

        let ran = ran(&child(&scratch)
            .env("MOKMOK_SCRIPT", "answers.json")
            .args(&args)
            .output()
            .expect("the binary runs"));

        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(
            std::fs::read_to_string(scratch.checkout().join("out.txt")).expect("the command wrote"),
            "hello",
        );
    }

    #[test]
    fn the_credential_the_env_key_names_reaches_the_provider() {
        let scratch = Scratch::new("credential-env");
        let path = scratch.0.join("script.json");
        script(&path, &[json!({"action": "final", "say": "done"})]);
        let (port, seen) = stub_endpoint();
        let endpoint = format!("http://127.0.0.1:{port}/v1");

        let args = agent_args(
            &scratch,
            &path,
            &[
                "--provider",
                "ollama",
                "--model",
                "kimi-k2.7-code",
                "--endpoint",
                &endpoint,
                "--env-key",
                "MOKMOK_CREDENTIAL",
            ],
        );
        let ran = ran(&child(&scratch)
            .env("MOKMOK_CREDENTIAL", "s3cr3t")
            .args(&args)
            .output()
            .expect("the binary runs"));

        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert!(
            ran.out.contains("phase=Closed(Completed)"),
            "the run did not close completed:\n{}",
            ran.out,
        );
        assert_eq!(
            seen.lock().unwrap().as_deref(),
            Some("Bearer s3cr3t"),
            "the credential the environment held did not reach the endpoint",
        );
    }

    #[test]
    fn a_run_refuses_a_reference_the_environment_does_not_answer() {
        let scratch = Scratch::new("config-variable-unset");
        let directory = scratch.0.join("config/mokmok");
        std::fs::create_dir_all(&directory).expect("the config directory is made");
        let file = directory.join("config.toml");
        std::fs::write(
            &file,
            "provider = \"scripted\"\nscript = \"${MOKMOK_SCRIPT}\"\n",
        )
        .expect("the config file is written");

        let mut args = base(&scratch);
        args.extend([
            "status".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
        ]);

        let ran = ran(&child(&scratch)
            .env_remove("MOKMOK_SCRIPT")
            .args(&args)
            .output()
            .expect("the binary runs"));

        assert!(!ran.ok, "{}\n{}", ran.out, ran.err);
        for expected in [
            file.display().to_string(),
            "script".to_owned(),
            "MOKMOK_SCRIPT".to_owned(),
        ] {
            assert!(ran.err.contains(&expected), "{}", ran.err);
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

        let crashed = run(
            &scratch,
            &agent_args(
                &scratch,
                &path,
                &["--crash-after", "before-dispatch", "--steps", "3"],
            ),
        );
        assert_eq!(crashed.code, Some(97), "{}\n{}", crashed.out, crashed.err);
        assert!(
            !scratch.checkout().join("marker.txt").exists(),
            "the crash stopped before anything ran:\n{}",
            crashed.out,
        );

        // The resumed model answers final. An automatic re-dispatch of the in-flight command would
        // leave the marker behind, so its absence is the measurement.
        script(&path, &[json!({"action": "final"})]);
        let resumed = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(resumed.ok, "{}\n{}", resumed.out, resumed.err);
        assert!(
            !scratch.checkout().join("marker.txt").exists(),
            "the resume never re-dispatched the command:\n{}",
            resumed.out,
        );

        let context = prompt_context(&scratch, 2);

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
        let opened = run(&scratch, &agent_args(&scratch, &path, &["--steps", "1"]));
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
        let cancelled = run(&scratch, &cancel);
        assert!(cancelled.ok, "{}\n{}", cancelled.out, cancelled.err);

        let closed = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(closed.ok, "{}\n{}", closed.out, closed.err);
        assert_eq!(
            status(&scratch).value("phase").as_deref(),
            Some("Closed(Cancelled)"),
        );

        let refused = run(&scratch, &submit_args(&scratch, "printf hello > out.txt"));
        assert!(
            refused.err.contains("the session is \"closed\""),
            "a closed session admits no new turn and names the phase it is in:\n{}",
            refused.err,
        );
    }

    #[test]
    fn a_submit_into_a_cancelled_session_is_refused_and_names_the_phase() {
        let scratch = Scratch::new("submit-draining");
        let submitted = run(&scratch, &submit_args(&scratch, "printf hello > out.txt"));
        assert!(submitted.ok, "{}\n{}", submitted.out, submitted.err);
        assert_eq!(submitted.value("phase").as_deref(), Some("Accepting"));

        let mut cancel = base(&scratch);
        cancel.extend([
            "agent".to_owned(),
            "cancel".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
        ]);
        let cancelled = run(&scratch, &cancel);
        assert!(cancelled.ok, "{}\n{}", cancelled.out, cancelled.err);

        // The cancel landed while the session was open, so the next invocation is the one that
        // refuses a new turn rather than the run that closes what the cancel left behind.
        let again = run(&scratch, &submit_args(&scratch, "printf hello > out.txt"));
        assert!(
            again.err.contains("the session is \"draining\""),
            "a cancelled session admits no new turn and names the phase it is in:\n{}",
            again.err,
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

        let ran = run(&scratch, &args);
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

        let ran = run(&scratch, &agent_args(&scratch, &path, &[]));
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

    /// Steps `driver` until it waits for the human or closes, and says which happened. A bound is
    /// what keeps a driver that never advances from spinning here forever: a test that hangs
    /// reports neither what it expected nor what it saw.
    async fn settled(driver: &mut Driver) -> bool {
        for _ in 0..20 {
            match driver.step().await.expect("the step runs") {
                Progress::Awaiting => return true,
                Progress::Terminal(_) => return false,
                Progress::Yielded | Progress::Advanced(_) => {},
            }
        }

        panic!("the driver neither waited nor closed within twenty steps");
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
        let config = Config::resolve(&parsed, None, &ProcessEnvironment::default())
            .expect("the configuration resolves");
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

        let awaiting = settled(&mut driver).await;
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
    async fn a_driver_answers_the_records_its_log_holds() {
        let scratch = Scratch::new("driver-records");
        let path = scratch.0.join("script.json");
        script(&path, &[json!({"action": "final", "say": "hi there"})]);
        let (config, run) = driver_config(&scratch, &path);
        let mut driver =
            Driver::open(&config, &run, Moment::from_millis(0)).expect("the driver opens");

        assert!(
            driver.records().expect("the ledger reads").is_empty(),
            "a driver that opened an empty log reports no records",
        );

        driver.speak("hello").expect("the line opens turn 1");
        assert!(settled(&mut driver).await, "the turn settled");

        let records = driver.records().expect("the ledger reads");
        assert!(
            records
                .iter()
                .any(|record| matches!(record, Record::Task(_))),
            "the records the driver reports are the ones its log holds: {records:?}",
        );
        assert!(
            records
                .iter()
                .any(|record| matches!(record, Record::Speech(_))),
            "{records:?}",
        );
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

        let awaiting = settled(&mut driver).await;
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

        let ran = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(ran.value("halt").as_deref(), Some("Attempts"));
        assert_eq!(
            status(&scratch).value("phase").as_deref(),
            Some("Closed(Completed)"),
        );
    }

    #[test]
    fn a_resumed_session_keeps_the_root_the_recorded_task_granted() {
        let scratch = Scratch::new("find-resumed");
        std::fs::write(
            scratch.checkout().join("notes.txt"),
            "the quokka walks at dawn\n",
        )
        .expect("the file is written");
        let path = scratch.0.join("script.json");

        script(&path, &[]);
        let root = scratch.checkout().display().to_string();
        let opened = run(
            &scratch,
            &agent_args(
                &scratch,
                &path,
                &["--search-root", root.as_str(), "--steps", "1"],
            ),
        );
        assert!(opened.ok, "{}\n{}", opened.out, opened.err);

        script(
            &path,
            &[
                json!({"action": "find", "query": "quokka"}),
                json!({"action": "final"}),
            ],
        );
        let resumed = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(resumed.ok, "{}\n{}", resumed.out, resumed.err);
        assert!(
            resumed.phrase("move=find ").is_some(),
            "the resumed run found rather than answered nothing:\n{}",
            resumed.out,
        );

        let context = prompt_context(&scratch, 2);
        assert!(
            context.contains("notes.txt") && context.contains("hits=1"),
            "the resumed finder searched the root the task recorded:\n{context}",
        );
    }

    #[test]
    fn a_find_reaches_the_next_prompt_with_the_one_file_it_ranked_first() {
        let scratch = Scratch::new("find-hit");
        std::fs::write(
            scratch.checkout().join("notes.txt"),
            "the quokka walks at dawn\n",
        )
        .expect("the file is written");
        std::fs::write(scratch.checkout().join("other.txt"), "nothing to see\n")
            .expect("the file is written");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "find", "query": "quokka"}),
                json!({"action": "final", "say": "found it"}),
            ],
        );
        let root = scratch.checkout().display().to_string();

        let ran = run(
            &scratch,
            &agent_args(&scratch, &path, &["--search-root", root.as_str()]),
        );
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(ran.value("phase").as_deref(), Some("Closed(Completed)"));

        let report = status(&scratch);
        let find = report
            .phrase("find ")
            .unwrap_or_else(|| panic!("the transcript holds the find:\n{}", report.out));
        assert!(find.contains("number=1"), "{find}");
        assert!(find.contains("query=\"quokka\""), "{find}");

        let context = prompt_context(&scratch, 2);
        assert!(
            context.contains("notes.txt"),
            "the hit reached the model in the next turn's prompt:\n{context}",
        );
        assert!(
            context.contains("hits=1"),
            "the header says the ranking kept one of the two files:\n{context}",
        );
    }

    #[test]
    fn a_hitless_find_settles_the_turn_completed() {
        let scratch = Scratch::new("find-hitless");
        std::fs::write(scratch.checkout().join("notes.txt"), "nothing to see\n")
            .expect("the file is written");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "find", "query": "petrichor"}),
                json!({"action": "final"}),
            ],
        );
        let root = scratch.checkout().display().to_string();

        let ran = run(
            &scratch,
            &agent_args(&scratch, &path, &["--search-root", root.as_str()]),
        );
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);

        let (log, session) = log_of(&scratch);
        let find = find_of(&log);

        assert!(
            find.report.observation.contains("hits=0"),
            "the observation says the search found nothing: {}",
            find.report.observation,
        );
        assert_eq!(
            log.turn(turn_id(session, 1))
                .expect("turn 1 is in the log")
                .outcome(),
            Some(TurnOutcome::Completed),
            "a search that found nothing settles the turn completed, not failed",
        );
    }

    #[test]
    fn a_session_with_no_granted_root_is_offered_no_finder() {
        let scratch = Scratch::new("find-no-root");
        let path = scratch.0.join("script.json");
        script(&path, &[json!({"action": "final", "say": "done"})]);

        let ran = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(ran.value("phase").as_deref(), Some("Closed(Completed)"));

        let first = prompt_context(&scratch, 1);
        assert!(
            !first.contains("find_files"),
            "no finder is named to a session that holds no root:\n{first}",
        );
        assert!(
            first.contains("\"search\":false"),
            "the context carries the flag the tool list is built from:\n{first}",
        );

        let report = status(&scratch);
        assert!(
            report.phrase("find ").is_none(),
            "the transcript holds no find record:\n{}",
            report.out,
        );
        let (log, _) = log_of(&scratch);
        assert!(
            !log.ordered
                .iter()
                .any(|record| matches!(record, Record::Find(_))),
            "the log holds no find record",
        );
    }

    #[test]
    fn a_find_answers_no_search_root_when_the_session_was_granted_none() {
        let scratch = Scratch::new("find-ungranted-nothing");
        let path = scratch.0.join("script.json");
        // The model asks for a finder this session was never offered, which is what a provider
        // whose context and tool list agreed would never do; the finder still answers.
        script(
            &path,
            &[
                json!({"action": "find", "query": "quokka"}),
                json!({"action": "final"}),
            ],
        );

        let ran = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(ran.value("phase").as_deref(), Some("Closed(Completed)"));

        let (log, session) = log_of(&scratch);

        assert_eq!(find_of(&log).report.observation, NO_SEARCH_ROOT);
        assert_eq!(
            log.turn(turn_id(session, 1))
                .expect("turn 1 is in the log")
                .outcome(),
            Some(TurnOutcome::Completed),
        );
    }

    #[test]
    fn a_search_root_no_grant_covers_is_refused_before_anything_opens() {
        let scratch = Scratch::new("find-ungranted");
        let elsewhere = scratch.0.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("the ungranted directory is made");
        let path = scratch.0.join("script.json");
        script(&path, &[json!({"action": "final"})]);
        let root = elsewhere.display().to_string();

        let refused = run(
            &scratch,
            &agent_args(&scratch, &path, &["--search-root", root.as_str()]),
        );

        assert!(!refused.ok, "{}", refused.out);
        assert!(refused.err.contains(&root), "{}", refused.err);
        assert!(refused.err.contains("grant"), "{}", refused.err);
        assert!(
            !scratch.data().join("events.sqlite").exists(),
            "the run was refused before the log opened:\n{}",
            refused.out,
        );
        assert!(
            !scratch.data().join("search").exists(),
            "the run was refused before any index opened:\n{}",
            refused.out,
        );
    }

    #[test]
    fn a_crashed_agent_run_leaves_its_workspace_under_the_configured_root() {
        let scratch = Scratch::new("sandbox-root");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[json!({"action": "command", "command": "printf hello"})],
        );

        let crashed = run(
            &scratch,
            &agent_args(&scratch, &path, &["--crash-after", "after-dispatch"]),
        );

        assert_eq!(crashed.code, Some(97), "{}\n{}", crashed.out, crashed.err);
        let left =
            std::fs::read_dir(scratch.data().join("sandbox")).map_or(0, std::fs::ReadDir::count);
        assert_eq!(
            left, 1,
            "the one workspace the crash left behind is under the root the operator's data directory names",
        );
    }

    #[test]
    fn a_commands_standard_output_reaches_the_observation_the_model_is_shown() {
        let scratch = Scratch::new("observation");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "command", "command": "printf hello"}),
                json!({"action": "final"}),
            ],
        );

        let ran = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);

        let (log, _) = log_of(&scratch);
        let observation = log
            .ordered
            .iter()
            .find_map(|record| match record {
                Record::Result { report, .. } => Some(report.observation.as_str()),
                _ => None,
            })
            .expect("the log holds the command's report");

        assert!(
            observation.contains("stdout: hello"),
            "the bytes the workload wrote are what the model is shown: {observation}",
        );
    }

    #[test]
    fn a_tool_runs_under_the_limits_the_operator_named() {
        let scratch = Scratch::new("limits");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "command", "command": "ulimit -v; ulimit -n; ulimit -f"}),
                json!({"action": "final"}),
            ],
        );

        // `ulimit -v` prints KiB and `ulimit -f` prints 512-byte blocks, so the unit conversions
        // show up here rather than being asserted by construction.
        let ran = run(
            &scratch,
            &agent_args(
                &scratch,
                &path,
                &[
                    "--address-space-bytes",
                    "67108864",
                    "--open-files",
                    "64",
                    "--file-size-bytes",
                    "1048576",
                ],
            ),
        );
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);

        let (log, _) = log_of(&scratch);
        let observation = log
            .ordered
            .iter()
            .find_map(|record| match record {
                Record::Result { report, .. } => Some(report.observation.as_str()),
                _ => None,
            })
            .expect("the log holds the command's report");

        assert!(
            observation.contains("stdout: 65536\n64\n2048"),
            "the workload did not run under the limits the operator named: {observation}",
        );
    }

    #[test]
    fn the_step_lines_name_the_answer_and_the_turn_the_log_gave_them() {
        let scratch = Scratch::new("step-lines");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "command", "command": "printf hello"}),
                json!({"action": "command", "command": "printf world"}),
                json!({"action": "final"}),
            ],
        );

        let ran = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);

        assert!(
            ran.out
                .contains("move=ask turn=1 attempt=1 answer=command command="),
            "the first step names the command the scripted model answered with:\n{}",
            ran.out,
        );
        assert!(
            ran.out.contains("move=act turn=1 "),
            "the first command ran under the turn the log opened for it:\n{}",
            ran.out,
        );
        assert!(
            ran.out.contains("move=act turn=2 "),
            "and the second command ran under the second turn:\n{}",
            ran.out,
        );
    }

    #[test]
    fn the_close_step_reports_the_ending_it_carried() {
        let scratch = Scratch::new("close-ending");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "command", "command": "printf hello"}),
                json!({"action": "final"}),
            ],
        );

        // A first run over a session of its own says how many steps the loop takes. The second run
        // stops on the step that carried the ending, because the step after it reaches the terminal
        // progress and answers the same ending whatever the close step recorded.
        let counted = run(
            &scratch,
            &agent_args_of(COUNTED, &scratch, &path, &["--steps", "20"]),
        );
        assert!(counted.ok, "{}\n{}", counted.out, counted.err);
        assert_eq!(counted.value("phase").as_deref(), Some("Closed(Completed)"));
        let close = counted
            .out
            .lines()
            // The session line is the one line the binary prints that is not a step.
            .skip(1)
            .position(|line| line.starts_with("move=close") && line.contains("ending=Some("))
            .expect("the counted run closed the session")
            + 1;

        let pinned = {
            let steps = close.to_string();

            run(
                &scratch,
                &agent_args_of(SESSION, &scratch, &path, &["--steps", steps.as_str()]),
            )
        };

        assert_eq!(
            pinned.value("phase").as_deref(),
            Some("Closed(Completed)"),
            "the step that carried the ending reported it:\n{}",
            pinned.out,
        );
    }

    #[tokio::test]
    async fn a_reopened_driver_reads_what_another_writer_committed() {
        let scratch = Scratch::new("reopen");
        let path = scratch.0.join("script.json");
        script(&path, &[json!({"action": "final", "say": "hi there"})]);
        let (config, run) = driver_config(&scratch, &path);
        let mut driver =
            Driver::open(&config, &run, Moment::from_millis(0)).expect("the driver opens");
        let session = driver.session();

        driver.speak("hello").expect("the line opens turn 1");
        assert!(settled(&mut driver).await, "the first line settled");

        let mut other =
            Ledger::open(&config, session, None, Moment::from_millis(0)).expect("the ledger opens");
        speak(&mut other, session, "from elsewhere").expect("the second line lands");

        assert_eq!(
            driver
                .transcript()
                .expect("the fold reads")
                .speech(turn_id(session, 2)),
            None,
            "a driver that opened before the write does not see it",
        );

        driver.reopen().expect("the ledger re-opens");

        assert_eq!(
            driver
                .transcript()
                .expect("the fold reads")
                .speech(turn_id(session, 2)),
            Some("from elsewhere"),
            "and re-opening is what reads it",
        );
    }

    const EDIT_PATCH: &str =
        "--- a/out.txt\n+++ b/out.txt\n@@ -1,2 +1,2 @@\n hello\n-world\n+moon\n";

    #[test]
    fn a_scripted_edit_lands_the_bytes_and_records_the_path_and_observation() {
        let scratch = Scratch::new("edit");
        let target = scratch.checkout().join("out.txt");
        std::fs::write(&target, "hello\nworld\n").expect("the seed file is written");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "edit", "patch": EDIT_PATCH}),
                json!({"action": "final", "say": "done"}),
            ],
        );

        let ran = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(
            std::fs::read_to_string(&target).expect("the file reads"),
            "hello\nmoon\n",
        );
        assert!(
            ran.out.contains("move=edit turn=1"),
            "the step line names the edit:\n{}",
            ran.out,
        );

        let report = status(&scratch);
        assert!(report.ok, "{}\n{}", report.out, report.err);
        assert!(
            report
                .out
                .lines()
                .any(|line| line.starts_with("edit ") && line.contains("relative=\"out.txt\"")),
            "the transcript's intent record carries the relative path:\n{}",
            report.out,
        );
        assert!(
            report
                .out
                .lines()
                .any(|line| line.starts_with("edit ") && line.contains("outcome=Applied")),
            "the transcript's outcome record says the edit applied:\n{}",
            report.out,
        );

        let (log, _) = log_of(&scratch);
        let intent = edit_intent_of(&log);
        assert_eq!(intent.relative, "out.txt");
        let outcome = edit_outcome_of(&log);
        assert!(matches!(outcome.report.outcome, EditOutcome::Applied));
        assert!(
            outcome
                .report
                .observation
                .contains("edit applied to out.txt"),
            "{}",
            outcome.report.observation,
        );
        assert!(
            outcome.report.observation.contains("sha256"),
            "the observation names both hashes:\n{}",
            outcome.report.observation,
        );
    }

    #[test]
    fn a_refused_hunk_leaves_the_file_unchanged_and_records_the_refusal() {
        let scratch = Scratch::new("edit-refused");
        let target = scratch.checkout().join("out.txt");
        std::fs::write(&target, "one\ntwo\n").expect("the seed file is written");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "edit", "patch": "--- a/out.txt\n+++ b/out.txt\n@@ -1,2 +1,2 @@\n three\n-two\n+four\n"}),
                json!({"action": "final", "say": "done"}),
            ],
        );

        let ran = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(
            std::fs::read_to_string(&target).expect("the file reads"),
            "one\ntwo\n",
            "a refused edit must not have written anything",
        );

        let report = status(&scratch);
        assert!(report.ok, "{}\n{}", report.out, report.err);
        assert!(
            report
                .out
                .lines()
                .any(|line| line.starts_with("edit ") && line.contains("outcome=Refused")),
            "the transcript records the refusal:\n{}",
            report.out,
        );

        let (log, session) = log_of(&scratch);
        assert_eq!(
            log.turn(turn_id(session, 1))
                .expect("turn 1 is in the log")
                .outcome(),
            Some(TurnOutcome::Failed),
            "a refused hunk fails the turn",
        );
    }

    #[test]
    fn a_replayed_edit_intent_performs_no_second_write() {
        let scratch = Scratch::new("edit-replay");
        let target = scratch.checkout().join("out.txt");
        std::fs::write(&target, "hello\nworld\n").expect("the seed file is written");
        let path = scratch.0.join("script.json");
        script(&path, &[json!({"action": "edit", "patch": EDIT_PATCH})]);

        // Three steps reach the edit's commit.
        let crashed = run(
            &scratch,
            &agent_args(
                &scratch,
                &path,
                &["--crash-after", "before-dispatch", "--steps", "3"],
            ),
        );
        assert_eq!(crashed.code, Some(97), "{}\n{}", crashed.out, crashed.err);
        assert_eq!(
            std::fs::read_to_string(&target).expect("the file reads"),
            "hello\nworld\n",
            "the crash stopped before the write:\n{}",
            crashed.out,
        );

        let (log, _) = log_of(&scratch);
        edit_intent_of(&log);
        assert!(
            !log.ordered
                .iter()
                .any(|record| matches!(record, Record::EditOutcome(_))),
            "the log holds the intent but no outcome",
        );

        let reconciled = reconcile(&scratch);
        assert!(reconciled.ok, "{}\n{}", reconciled.out, reconciled.err);
        assert!(
            reconciled.out.contains("resolution=Unknown"),
            "reconcile settles the execution unknown:\n{}",
            reconciled.out,
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("the file reads"),
            "hello\nworld\n",
            "reconcile did not re-apply the patch",
        );

        script(&path, &[json!({"action": "final", "say": "done"})]);
        let resumed = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(resumed.ok, "{}\n{}", resumed.out, resumed.err);

        let context = prompt_context(&scratch, 2);
        assert!(
            context.contains("interrupted"),
            "the next prompt carries the unproven warning:\n{context}",
        );
        assert!(
            context.contains("sha256"),
            "and names both hashes:\n{context}",
        );
    }

    #[test]
    fn a_crash_after_dispatch_in_an_edit_keeps_the_post_image_and_settles_unknown() {
        let scratch = Scratch::new("edit-crash-late");
        let target = scratch.checkout().join("out.txt");
        std::fs::write(&target, "hello\nworld\n").expect("the seed file is written");
        let path = scratch.0.join("script.json");
        script(&path, &[json!({"action": "edit", "patch": EDIT_PATCH})]);

        // Three steps reach the edit's commit.
        let crashed = run(
            &scratch,
            &agent_args(
                &scratch,
                &path,
                &["--crash-after", "after-dispatch", "--steps", "3"],
            ),
        );
        assert_eq!(crashed.code, Some(97), "{}\n{}", crashed.out, crashed.err);
        assert_eq!(
            std::fs::read_to_string(&target).expect("the file reads"),
            "hello\nmoon\n",
            "the file holds the post-image after the crash:\n{}",
            crashed.out,
        );

        let (log, _) = log_of(&scratch);
        edit_intent_of(&log);
        assert!(
            !log.ordered
                .iter()
                .any(|record| matches!(record, Record::EditOutcome(_))),
            "the log holds the intent but no outcome",
        );

        let reconciled = reconcile(&scratch);
        assert!(reconciled.ok, "{}\n{}", reconciled.out, reconciled.err);
        assert!(
            reconciled.out.contains("resolution=Unknown"),
            "reconcile settles the execution unknown:\n{}",
            reconciled.out,
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("the file reads"),
            "hello\nmoon\n",
            "reconcile did not re-apply the patch",
        );

        script(&path, &[json!({"action": "final", "say": "done"})]);
        let resumed = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(resumed.ok, "{}\n{}", resumed.out, resumed.err);

        let context = prompt_context(&scratch, 2);
        assert!(
            context.contains("interrupted"),
            "the next prompt carries the unproven warning:\n{context}",
        );
        assert!(
            context.contains("sha256"),
            "and names both hashes:\n{context}",
        );
    }

    #[test]
    fn a_scripted_subagent_spawns_a_child_session_and_returns_its_final_reply() {
        let scratch = Scratch::new("subagent");
        let path = scratch.0.join("script.json");
        let command = format!(
            "cd {} && printf hello > child.txt",
            scratch.checkout().display(),
        );
        script(
            &path,
            &[
                // Parent turn 1: delegate to a child.
                json!({
                    "action": "subagent",
                    "goal": "write hello into child.txt in the checkout",
                }),
                // Child turn 1: run the command.
                json!({"action": "command", "command": command}),
                // Child turn 2: finish.
                json!({"action": "final", "say": "the child wrote the file"}),
                // Parent turn 2: finish with the child's reply as the human-facing prose.
                json!({"action": "final", "say": "the child wrote the file"}),
            ],
        );

        let ran = run(&scratch, &agent_args(&scratch, &path, &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(
            std::fs::read_to_string(scratch.checkout().join("child.txt"))
                .expect("the child command wrote"),
            "hello",
        );

        let (parent_log, parent_session) = log_of(&scratch);
        let intent = subagent_intent_of(&parent_log);
        assert_eq!(intent.goal, "write hello into child.txt in the checkout");
        let outcome = subagent_outcome_of(&parent_log);
        assert_eq!(outcome.reply, "the child wrote the file");

        let child_session = intent.child_session;
        assert_ne!(
            child_session, parent_session,
            "the child session is not the parent session",
        );
        let child_db = Db::open(&scratch.data().join("events.sqlite")).expect("the log opens");
        let child_log = child_db.log(child_session).expect("the child stream reads");
        assert!(
            child_log
                .ordered
                .iter()
                .any(|record| matches!(record, Record::Task(_))),
            "the child stream holds its task",
        );
        assert!(
            child_log.ordered.iter().any(|record| match record {
                Record::Answer(answer) => {
                    answer.action == Action::Final && answer.prose == "the child wrote the file"
                },
                _ => false,
            }),
            "the child stream holds its final answer",
        );

        let report = status(&scratch);
        assert!(report.ok, "{}\n{}", report.out, report.err);
        let final_answer = report
            .out
            .lines()
            .find(|line| line.starts_with("answer ") && line.contains("action=Final"))
            .expect("the final answer is reported");
        assert!(
            final_answer.contains("say=\"the child wrote the file\""),
            "the parent's final prose is the human reply: {final_answer}",
        );
    }
}
