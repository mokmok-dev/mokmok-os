//! Skills, driven the way an operator drives them: through `mokmokd`, with a scripted provider and a
//! real SQLite log, so discovery, the frozen advertisement, activation, and the refusal of a changed
//! file are the whole surface.
//!
//! No test here reaches the network: the scripted provider answers from a file.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use mokmok_agent_runtime::{Record, SkillOutcome};
    use mokmok_artifact::{BlobStore, ContentHash};
    use mokmok_kernel::{Db, Log, uuid_of};
    use mokmok_protocol::SessionID;

    const SESSION: &str = "018f2f1e-0000-7000-8000-000000000000";

    /// A directory of this test's own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mokmok-skill-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join("checkout")).expect("the scratch checkout is made");

            Self(path)
        }

        fn data(&self) -> PathBuf {
            self.0.join("data")
        }

        fn checkout(&self) -> PathBuf {
            self.0.join("checkout")
        }

        /// A skill root with `directory/SKILL.md` holding `text`.
        fn skill(
            &self,
            root: &str,
            directory: &str,
            text: &str,
        ) -> PathBuf {
            let root = self.0.join(root);
            let directory = root.join(directory);
            std::fs::create_dir_all(&directory).expect("the skill directory is made");
            std::fs::write(directory.join("SKILL.md"), text).expect("the skill is written");

            root
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

    fn ran(output: &Output) -> Ran {
        Ran {
            ok: output.status.success(),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn child(scratch: &Scratch) -> Command {
        let mut command = Command::new(binary());
        command
            .env("MOKMOKD_CRASH", "1")
            .env("XDG_CONFIG_HOME", scratch.0.join("config"));

        command
    }

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

    /// One `agent run` for the scratch session, with the named skill roots.
    fn agent_args(
        scratch: &Scratch,
        script_path: &Path,
        roots: &[&Path],
        extra: &[&str],
    ) -> Vec<String> {
        let mut args = base(scratch);
        args.extend([
            "agent".to_owned(),
            "run".to_owned(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--goal".to_owned(),
            "use the advertised skill".to_owned(),
            "--checkout".to_owned(),
            scratch.checkout().display().to_string(),
            "--script".to_owned(),
            script_path.display().to_string(),
            "--max-turns".to_owned(),
            "6".to_owned(),
        ]);
        for root in roots {
            args.push("--skill-root".to_owned());
            args.push(root.display().to_string());
        }
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

    /// The stored context bytes of one turn's prompt, as JSON text.
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

    /// The task's own record, so a test can read the skill record that was folded.
    fn log_of(scratch: &Scratch) -> Log {
        let db = Db::open(&scratch.data().join("events.sqlite")).expect("the log opens");
        let session =
            SessionID::from_uuid(uuid_of(SESSION, "session").expect("the session is a uuid"));

        db.log(session).expect("the log reads")
    }

    /// The one skill record the session folded.
    fn skill_record(log: &Log) -> &mokmok_agent_runtime::SkillRecord {
        log.ordered
            .iter()
            .find_map(|record| match record {
                Record::Skill(skill) => Some(skill),
                _ => None,
            })
            .expect("the session holds one skill record")
    }

    #[test]
    fn a_valid_skill_is_advertised_and_an_invalid_one_is_skipped() {
        let scratch = Scratch::new("advertise");
        let valid = scratch.skill(
            "skills",
            "pdf-processing",
            "---\nname: pdf-processing\ndescription: Extract PDF text.\n---\ndo the thing\n",
        );
        scratch.skill(
            "skills",
            "broken",
            "---\nname: Broken\ndescription: d\n---\nbody\n",
        );
        std::fs::create_dir_all(scratch.0.join("skills/empty")).expect("a bare directory");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[serde_json::json!({"action": "final", "say": "done"})],
        );

        let ran = run(&scratch, &agent_args(&scratch, &path, &[&valid], &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);

        let context = prompt_context(&scratch, 1);
        assert!(
            context.contains("- pdf-processing: Extract PDF text."),
            "the valid skill is advertised:\n{context}",
        );
        assert!(
            context.contains("\"skills\":true"),
            "the context offers the tool the advertisement is built with:\n{context}",
        );
        assert!(
            !context.contains("Broken"),
            "an invalid skill is skipped rather than advertised:\n{context}",
        );
    }

    #[test]
    fn a_folded_description_advertises_folded() {
        let scratch = Scratch::new("folded");
        // A folded description between other keys, which is the shape a real skill uses and the one
        // discovery must not spin on.
        let root = scratch.skill(
            "skills",
            "folded",
            "---\nname: folded\ndescription: >-\n  Extract PDF text,\n  fill forms, merge files.\nlicense: MIT\ncompatibility: >-\n  Requires uv.\n---\nbody\n",
        );
        let path = scratch.0.join("script.json");
        script(&path, &[serde_json::json!({"action": "final"})]);

        let ran = run(&scratch, &agent_args(&scratch, &path, &[&root], &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);

        let context = prompt_context(&scratch, 1);
        assert!(
            context.contains("- folded: Extract PDF text, fill forms, merge files."),
            "the folded description is joined with a space before it is advertised:\n{context}",
        );
    }

    #[test]
    fn a_scripted_run_loads_a_skill_and_the_next_prompt_carries_its_instructions() {
        let scratch = Scratch::new("load");
        let root = scratch.skill(
            "skills",
            "pdf-processing",
            "---\nname: pdf-processing\ndescription: Extract PDF text.\n---\nExtract PDF text.\n",
        );
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                serde_json::json!({"action": "use_skill", "name": "pdf-processing"}),
                serde_json::json!({"action": "final", "say": "loaded"}),
            ],
        );

        let ran = run(&scratch, &agent_args(&scratch, &path, &[&root], &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        assert_eq!(ran.value("phase").as_deref(), Some("Closed(Completed)"));

        let report = status(&scratch);
        assert!(report.ok, "{}\n{}", report.out, report.err);
        let skill = report
            .phrase("skill ")
            .unwrap_or_else(|| panic!("the transcript holds the skill record:\n{}", report.out));
        assert!(skill.contains("pdf-processing"), "{skill}");
        assert!(skill.contains("loaded"), "{skill}");

        let context = prompt_context(&scratch, 2);
        assert!(
            context.contains("Extract PDF text."),
            "the loaded instructions ride the next prompt:\n{context}",
        );
        assert!(
            context.contains("\"pdf-processing\""),
            "the loaded body is named by the skill it came from:\n{context}",
        );

        let log = log_of(&scratch);
        assert_eq!(
            skill_record(&log).outcome,
            SkillOutcome::Loaded {
                instructions: "Extract PDF text.\n".to_owned(),
            },
        );
    }

    #[test]
    fn a_skill_changed_after_discovery_is_refused_at_activation() {
        let scratch = Scratch::new("changed");
        let root = scratch.skill(
            "skills",
            "pdf-processing",
            "---\nname: pdf-processing\ndescription: Extract PDF text.\n---\nExtract PDF text.\n",
        );
        let path = scratch.0.join("script.json");

        script(&path, &[]);
        let opened = run(
            &scratch,
            &agent_args(&scratch, &path, &[&root], &["--steps", "1"]),
        );
        assert!(opened.ok, "{}\n{}", opened.out, opened.err);

        // The file changes after discovery, so activation must refuse the body it no longer matches.
        std::fs::write(
            root.join("pdf-processing/SKILL.md"),
            "---\nname: pdf-processing\ndescription: Extract PDF text.\n---\nA different body.\n",
        )
        .expect("the skill changes");

        script(
            &path,
            &[
                serde_json::json!({"action": "use_skill", "name": "pdf-processing"}),
                serde_json::json!({"action": "final"}),
            ],
        );
        let ran = run(&scratch, &agent_args(&scratch, &path, &[&root], &[]));
        assert!(ran.ok, "{}\n{}", ran.out, ran.err);

        let report = status(&scratch);
        let skill = report
            .phrase("skill ")
            .unwrap_or_else(|| panic!("the transcript holds the skill record:\n{}", report.out));
        assert!(skill.contains("refused"), "{skill}");

        let context = prompt_context(&scratch, 2);
        assert!(
            context.contains("changed after it was discovered"),
            "the refusal says the file no longer matches:\n{context}",
        );
        assert!(
            !context.contains("A different body."),
            "a changed body is never handed to the model:\n{context}",
        );

        let log = log_of(&scratch);
        assert!(
            matches!(
                &skill_record(&log).outcome,
                SkillOutcome::Refused { reason } if reason.contains("changed after it was discovered"),
            ),
            "{:?}",
            skill_record(&log).outcome,
        );
    }

    #[test]
    fn a_skill_root_that_is_not_a_directory_is_refused_at_resolve() {
        let scratch = Scratch::new("not-a-directory");
        let path = scratch.0.join("script.json");
        script(&path, &[serde_json::json!({"action": "final"})]);
        let file = path.display().to_string();

        let ran = run(
            &scratch,
            &agent_args(&scratch, &path, &[], &["--skill-root", file.as_str()]),
        );

        assert!(!ran.ok, "a file is not a skill root:\n{}", ran.out);
        assert!(
            ran.err.contains("is not a directory"),
            "the resolve names the offending path:\n{}",
            ran.err,
        );
    }

    #[test]
    fn two_roots_holding_one_name_are_refused() {
        let scratch = Scratch::new("duplicate");
        let first = scratch.skill(
            "one",
            "pdf-processing",
            "---\nname: pdf-processing\ndescription: d\n---\nbody\n",
        );
        let second = scratch.skill(
            "two",
            "pdf-processing",
            "---\nname: pdf-processing\ndescription: d\n---\nbody\n",
        );
        let path = scratch.0.join("script.json");
        script(&path, &[serde_json::json!({"action": "final"})]);

        let ran = run(
            &scratch,
            &agent_args(&scratch, &path, &[&first, &second], &[]),
        );

        assert!(!ran.ok, "two files cannot claim one name:\n{}", ran.out);
        assert!(
            ran.err.contains("held by both") && ran.err.contains("pdf-processing"),
            "the refusal names the name and both paths:\n{}",
            ran.err,
        );
    }
}
