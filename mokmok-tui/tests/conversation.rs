//! The conversation with no terminal at all: scripted keys, a scripted provider, and a real log.
//!
//! No test here reaches the network: the provider's answers come from a file, so a whole exchange —
//! the human line, the model call, the durable step — runs in the gate, which has neither a terminal
//! nor an endpoint.

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};

    use clap::Parser as _;
    use mokmok_agent_runtime::{Progress, turn_id};
    use mokmok_artifact::{BlobStore, ContentHash};
    use mokmok_kernel::{Agent, Args, Command, Config, Run};
    use mokmok_protocol::Moment;
    use mokmok_state_machine::{Phase, Turn, TurnOutcome};
    use mokmok_tui::{Engine, Error, Event, Key, Line, Screen, Session, View, run};
    use serde_json::json;

    const SESSION: &str = "018f2f1e-0000-7000-8000-000000000000";

    /// A directory of this test's own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mokmok-tui-{name}-{}", std::process::id()));
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

    /// A screen driven by a script of keys, remembering every frame it was handed.
    struct ScriptedScreen {
        keys: VecDeque<Key>,
        frames: Vec<View>,
    }

    impl Screen for ScriptedScreen {
        fn event(&mut self) -> Result<Event, Error> {
            // A screen with no keys left is a terminal nobody is typing at: `Error::Terminal` stops
            // the driver rather than letting it spin.
            self.keys.pop_front().map_or_else(
                || {
                    Err(Error::Terminal(std::io::Error::other(
                        "the script is spent",
                    )))
                },
                |key| Ok(Event::Key(key)),
            )
        }

        fn draw(
            &mut self,
            view: &View,
        ) -> Result<(), Error> {
            self.frames.push(view.clone());

            Ok(())
        }
    }

    /// The keys that type each line and submit it, then leave with `Ctrl-C`.
    fn typed(lines: &[&str]) -> VecDeque<Key> {
        let mut keys = VecDeque::new();

        for line in lines {
            for character in line.chars() {
                keys.push_back(Key::Char(character));
            }
            keys.push_back(Key::Enter);
        }
        keys.push_back(Key::Interrupt);

        keys
    }

    /// A `cargo mutants` run names the confiner with `MOKMOK_CONFINER`.
    ///
    /// The workspace's own confiner sits beside this crate's binary, and a package-scoped build
    /// leaves it out, because it belongs to another package. No case here dispatches a tool, so the
    /// path only has to name a runnable file, and `Config::resolve` refuses every grant without one.
    fn confiner() -> PathBuf {
        if let Some(named) = std::env::var_os("MOKMOK_CONFINER") {
            let path = PathBuf::from(named);
            assert!(path.is_file(), "{} is not a file", path.display());

            return path;
        }

        let sibling = PathBuf::from(env!("CARGO_BIN_EXE_mokmok-tui"))
            .parent()
            .expect("the binary has a directory")
            .join("mokmok-sandbox-confine");

        if sibling.is_file() { sibling } else { shell() }
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

    /// The validated configuration and run one conversation names, with `extra` appended.
    fn config_and_run(
        scratch: &Scratch,
        script_path: &Path,
        extra: &[&str],
    ) -> (Config, Run) {
        let mut args = vec!["mokmok-tui".to_owned()];
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
        args.extend(extra.iter().map(|value| (*value).to_owned()));
        let parsed = Args::try_parse_from(&args).expect("the command line parses");
        let config = Config::resolve(&parsed, None).expect("the configuration resolves");
        let Command::Agent(Agent::Run(run)) = parsed.command else {
            panic!("the command line named an agent run");
        };

        (config, run)
    }

    #[tokio::test]
    async fn a_conversation_shows_the_line_it_answers_and_survives_two_exchanges() {
        let scratch = Scratch::new("conversation");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "final", "say": "the first reply"}),
                json!({"action": "final", "say": "the second reply"}),
            ],
        );
        let (config, spec) = config_and_run(&scratch, &path, &[]);

        let mut session =
            Session::open(&config, &spec, Moment::from_millis(0)).expect("the session opens");
        let session_id = session.session();
        let mut screen = ScriptedScreen {
            keys: typed(&["line 1", "line 2"]),
            frames: Vec::new(),
        };

        let outcome = run(&mut session, &mut screen)
            .await
            .expect("the conversation ends when the human leaves");
        assert!(
            outcome.ending.is_none(),
            "Ctrl-C leaves the session open: {:?}",
            outcome.ending,
        );

        let shown: Vec<&Line> = screen
            .frames
            .iter()
            .flat_map(|frame| frame.lines.iter())
            .collect();
        assert!(
            shown
                .iter()
                .any(|line| **line == Line::Human("line 1".to_owned())),
            "the human's first line is on screen before it is answered",
        );
        assert!(
            shown
                .iter()
                .any(|line| **line == Line::Agent("the first reply".to_owned())),
            "the model's own words are on screen",
        );

        let fold = session.fold().expect("the fold reads");
        assert_eq!(fold.speeches.len(), 2, "both human lines are durable");
        assert_eq!(
            fold.speeches
                .get(&turn_id(session_id, 1))
                .map(String::as_str),
            Some("line 1"),
        );
        assert_eq!(
            fold.speeches
                .get(&turn_id(session_id, 2))
                .map(String::as_str),
            Some("line 2"),
            "the second line extends the one session rather than opening a new one",
        );
        assert_eq!(
            fold.session.phase(),
            Phase::Accepting,
            "the conversation is still open for its human",
        );

        let first = turn_id(session_id, 1);
        let prompt = fold
            .prompts
            .iter()
            .find(|((turn, _), _)| *turn == first)
            .map(|(_, reference)| reference)
            .expect("turn 1's prompt is recorded");
        let bytes = BlobStore::open(config.artifacts.clone())
            .expect("the artifact store opens")
            .read(prompt.hash.parse::<ContentHash>().expect("the hash is hex"))
            .expect("the prompt bytes are stored");
        let text = String::from_utf8(bytes).expect("the prompt is JSON text");

        assert!(
            text.contains("line 1"),
            "the prompt for turn 1 carries the line it answers:\n{text}",
        );
    }

    #[tokio::test]
    async fn a_model_that_never_answers_settles_the_turn_and_leaves_the_conversation_open() {
        let scratch = Scratch::new("unanswered");
        let path = scratch.0.join("script.json");
        script(&path, &[]);
        let (config, spec) = config_and_run(&scratch, &path, &[]);

        let mut session =
            Session::open(&config, &spec, Moment::from_millis(0)).expect("the session opens");
        let session_id = session.session();
        let mut screen = ScriptedScreen {
            keys: typed(&["line 1"]),
            frames: Vec::new(),
        };

        let outcome = run(&mut session, &mut screen)
            .await
            .expect("a provider failure does not end the conversation");
        assert!(
            outcome.ending.is_none(),
            "Ctrl-C leaves the session open: {:?}",
            outcome.ending,
        );

        let shown: Vec<&Line> = screen
            .frames
            .iter()
            .flat_map(|frame| frame.lines.iter())
            .collect();
        assert!(
            shown
                .iter()
                .any(|line| **line == Line::Human("line 1".to_owned())),
            "the human's line is on screen",
        );
        assert!(
            shown
                .iter()
                .any(|line| **line
                    == Line::Trouble("the model did not answer in 3 attempts".to_owned())),
            "the spent attempts are on screen as trouble: {shown:?}",
        );

        let fold = session.fold().expect("the fold reads");
        assert_eq!(
            fold.session.phase(),
            Phase::Accepting,
            "a failed model call leaves the conversation open for its human",
        );

        let first = turn_id(session_id, 1);
        let prompts = fold
            .prompts
            .keys()
            .filter(|(turn, _)| *turn == first)
            .count();
        assert_eq!(
            prompts, 3,
            "the model is re-asked until the turn's attempt ceiling is spent",
        );
        assert_eq!(
            fold.turn(first).and_then(Turn::outcome),
            Some(TurnOutcome::Failed),
            "the turn settles failed rather than the session closing",
        );
    }

    #[tokio::test]
    async fn a_line_is_refused_while_the_turn_is_in_flight() {
        let scratch = Scratch::new("refusal");
        let path = scratch.0.join("script.json");
        script(&path, &[json!({"action": "command", "command": "true"})]);
        let (config, spec) = config_and_run(&scratch, &path, &[]);

        let mut session =
            Session::open(&config, &spec, Moment::from_millis(0)).expect("the session opens");
        session
            .speak("line 1")
            .expect("the first line opens turn 1");

        match session.step().await.expect("the turn is asked") {
            Progress::Advanced(_) | Progress::Yielded => {},
            other => panic!("the one step asks the model: {other:?}"),
        }

        let refused = session.speak("line 2");
        assert!(
            matches!(
                refused,
                Err(Error::Agent(mokmok_agent_runtime::Error::NotAwaiting))
            ),
            "a line folded into a turn already asked is refused: {refused:?}",
        );
    }

    #[tokio::test]
    async fn a_find_reaches_the_screen_as_a_find_and_not_as_a_command() {
        let scratch = Scratch::new("find");
        std::fs::write(
            scratch.checkout().join("notes.txt"),
            "the quokka walks at dawn\n",
        )
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
        let (config, spec) = config_and_run(&scratch, &path, &["--search-root", root.as_str()]);

        let mut session =
            Session::open(&config, &spec, Moment::from_millis(0)).expect("the session opens");
        let mut screen = ScriptedScreen {
            keys: typed(&["where is the quokka"]),
            frames: Vec::new(),
        };

        run(&mut session, &mut screen)
            .await
            .expect("the conversation ends when the human leaves");

        let shown: Vec<Line> = screen
            .frames
            .iter()
            .flat_map(|frame| frame.lines.iter())
            .cloned()
            .collect();
        let found = shown
            .iter()
            .find_map(|line| match line {
                Line::Find { query, observation } => Some((query.clone(), observation.clone())),
                _ => None,
            })
            .expect("the find is on screen");

        assert_eq!(found.0, "quokka");
        assert!(
            found.1.contains("hits=1") && found.1.contains("notes.txt"),
            "the find's own answer is on screen: {:?}",
            found.1,
        );
        assert!(
            !shown.iter().any(|line| matches!(line, Line::Work { .. })),
            "a find must not be drawn as a command the agent ran: {shown:?}",
        );
    }
}
