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
    use mokmok_protocol::{Moment, SessionID};
    use mokmok_state_machine::{Phase, Turn, TurnOutcome};
    use mokmok_tui::{
        Engine, Error, Event, Key, Line, Screen, Session, Sessions, Sheet, View, run,
    };
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

    #[derive(Clone, Copy, Debug)]
    enum Step {
        Key(Key),
        /// The screen reports nothing for this many polls, which is what the human not typing looks
        /// like while the agent works.
        Think(usize),
    }

    struct ScriptedScreen {
        script: VecDeque<Step>,
        frames: Vec<View>,
    }

    impl Screen for ScriptedScreen {
        fn poll(&mut self) -> Result<Option<Event>, Error> {
            match self.script.front_mut() {
                Some(Step::Think(left)) => {
                    *left -= 1;
                    if *left == 0 {
                        self.script.pop_front();
                    }

                    Ok(None)
                },
                Some(Step::Key(key)) => {
                    let key = *key;
                    self.script.pop_front();

                    Ok(Some(Event::Key(key)))
                },
                None => Err(Error::Terminal(std::io::Error::other(
                    "the script is spent",
                ))),
            }
        }

        fn event(&mut self) -> Result<Event, Error> {
            // A blocking wait skips the silences: they are what the human not typing looks like, and
            // the driver only blocks when nothing can move.
            loop {
                match self.script.pop_front() {
                    Some(Step::Think(_)) => {},
                    Some(Step::Key(key)) => return Ok(Event::Key(key)),
                    None => {
                        return Err(Error::Terminal(std::io::Error::other(
                            "the script is spent",
                        )));
                    },
                }
            }
        }

        fn draw(
            &mut self,
            view: &View,
        ) -> Result<(), Error> {
            self.frames.push(view.clone());

            Ok(())
        }
    }

    /// The keys that type each line and submit it, with a silence after it, and then the interrupt.
    ///
    /// The silence is what gives the session the loops it needs to answer, and a silence longer than
    /// the answer needs costs nothing: the driver skips the ones left once nothing can move.
    fn typed(lines: &[&str]) -> VecDeque<Step> {
        let mut steps = VecDeque::new();

        for line in lines {
            steps.extend(
                line.chars()
                    .map(|character| Step::Key(Key::Char(character))),
            );
            steps.push_back(Step::Key(Key::Enter));
            steps.push_back(Step::Think(32));
        }
        steps.push_back(Step::Key(Key::Interrupt));

        steps
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

    /// The fold of the session the log holds, read the way another process would read it.
    fn probe(
        config: &Config,
        run: &Run,
        session: SessionID,
    ) -> mokmok_agent_runtime::Transcript {
        let spec = Run {
            session: session.as_uuid().to_string(),
            ..run.clone()
        };

        Session::open(config, &spec, Moment::from_millis(0))
            .expect("the session re-opens")
            .fold()
            .expect("the fold reads")
    }

    /// Steps an engine until the model is done with it, so a case can leave a session behind.
    async fn settle(session: &mut Session) {
        for _ in 0..8 {
            match session.step().await.expect("the step runs") {
                Progress::Awaiting | Progress::Terminal(_) => return,
                _ => {},
            }
        }

        panic!("the session did not settle");
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
        let at = Moment::from_millis(0);
        let world = Sessions::new(config.clone(), spec.clone(), at);
        let first = world.start().expect("the session opens");
        let mut screen = ScriptedScreen {
            script: typed(&["line 1", "line 2"]),
            frames: Vec::new(),
        };

        let outcome = run(&world, first, &mut screen)
            .await
            .expect("the conversation ends when the human leaves");
        assert_eq!(outcome.left.len(), 1, "the process held one session");
        assert!(
            outcome.left[0].ending.is_none(),
            "Ctrl-C leaves the session open: {:?}",
            outcome.left[0].ending,
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

        let session_id = SessionID::from_uuid(
            SESSION
                .parse::<uuid::Uuid>()
                .expect("the session is a uuid"),
        );
        let fold = probe(&config, &spec, session_id);
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

        let first_turn = turn_id(session_id, 1);
        let prompt = fold
            .prompts
            .iter()
            .find(|((turn, _), _)| *turn == first_turn)
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
        let at = Moment::from_millis(0);
        let world = Sessions::new(config.clone(), spec.clone(), at);
        let first = world.start().expect("the session opens");
        let mut screen = ScriptedScreen {
            script: typed(&["line 1"]),
            frames: Vec::new(),
        };

        let outcome = run(&world, first, &mut screen)
            .await
            .expect("a provider failure does not end the conversation");
        assert!(
            outcome.left[0].ending.is_none(),
            "Ctrl-C leaves the session open: {:?}",
            outcome.left[0].ending,
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

        let session_id = SessionID::from_uuid(
            SESSION
                .parse::<uuid::Uuid>()
                .expect("the session is a uuid"),
        );
        let fold = probe(&config, &spec, session_id);
        assert_eq!(
            fold.session.phase(),
            Phase::Accepting,
            "a failed model call leaves the conversation open for its human",
        );

        let first_turn = turn_id(session_id, 1);
        let prompts = fold
            .prompts
            .keys()
            .filter(|(turn, _)| *turn == first_turn)
            .count();
        assert_eq!(
            prompts, 3,
            "the model is re-asked until the turn's attempt ceiling is spent",
        );
        assert_eq!(
            fold.turn(first_turn).and_then(Turn::outcome),
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
        let world = Sessions::new(config, spec, Moment::from_millis(0));
        let first = world.start().expect("the session opens");
        let mut screen = ScriptedScreen {
            script: typed(&["where is the quokka"]),
            frames: Vec::new(),
        };

        run(&world, first, &mut screen)
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

    #[tokio::test]
    async fn a_session_the_human_left_keeps_moving_while_he_is_in_another() {
        let scratch = Scratch::new("background");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "final", "say": "the first reply"}),
                json!({"action": "final", "say": "the second reply"}),
            ],
        );
        let (config, spec) = config_and_run(&scratch, &path, &[]);
        let world = Sessions::new(config, spec, Moment::from_millis(0));
        let first = world.start().expect("the session opens");

        let mut script = VecDeque::new();
        for character in "line 1".chars() {
            script.push_back(Step::Key(Key::Char(character)));
        }
        script.push_back(Step::Key(Key::Enter));
        script.push_back(Step::Key(Key::New));
        for character in "second".chars() {
            script.push_back(Step::Key(Key::Char(character)));
        }
        script.push_back(Step::Key(Key::Enter));
        script.push_back(Step::Think(32));
        script.push_back(Step::Key(Key::Interrupt));
        let mut screen = ScriptedScreen {
            script,
            frames: Vec::new(),
        };

        let outcome = run(&world, first, &mut screen)
            .await
            .expect("the conversation ends when the human leaves");

        assert_eq!(
            outcome.left.len(),
            2,
            "the process held the session it started in and the one the human created",
        );
        let started = &outcome.left[0];
        assert_eq!(started.session.as_uuid().to_string(), SESSION);
        assert!(
            started.lines.contains(&Line::Human("line 1".to_owned())),
            "the human's line is in the session he left: {:?}",
            started.lines,
        );
        assert!(
            started
                .lines
                .contains(&Line::Agent("the first reply".to_owned())),
            "the turn settled while the human was in the other session: {:?}",
            started.lines,
        );

        let created = outcome.left[1].session;
        let background = screen
            .frames
            .iter()
            .find(|frame| frame.session == created && frame.background.is_some())
            .unwrap_or_else(|| {
                panic!(
                    "no frame in the new session reported the one in the background: {:?}",
                    screen
                        .frames
                        .iter()
                        .map(|frame| (frame.session, frame.background.clone()))
                        .collect::<Vec<_>>(),
                )
            });

        assert_eq!(
            background.background.as_deref(),
            Some("1 other session working"),
        );
    }

    #[tokio::test]
    async fn the_listing_shows_the_logs_other_session_and_switching_to_it_opens_it() {
        let scratch = Scratch::new("listing");
        let path = scratch.0.join("script.json");
        script(
            &path,
            &[
                json!({"action": "final", "say": "the older reply"}),
                json!({"action": "final", "say": "the newer reply"}),
            ],
        );
        let (config, spec) = config_and_run(&scratch, &path, &[]);
        let at = Moment::from_millis(0);
        let elsewhere = SessionID::from_uuid(uuid::Uuid::from_u128(0x11));

        // A session another run left behind, written through the same world the conversation uses.
        {
            let older = Run {
                session: elsewhere.as_uuid().to_string(),
                ..spec.clone()
            };
            let mut session = Session::open(&config, &older, at).expect("the older session opens");
            session.speak("the older line").expect("it takes the line");
            settle(&mut session).await;
        }

        let world = Sessions::new(config, spec, at);
        let first = world.start().expect("the session opens");
        let mut screen = ScriptedScreen {
            script: VecDeque::from([
                Step::Key(Key::List),
                Step::Key(Key::Down),
                Step::Key(Key::Enter),
                Step::Think(8),
                Step::Key(Key::Interrupt),
            ]),
            frames: Vec::new(),
        };

        let outcome = run(&world, first, &mut screen)
            .await
            .expect("the conversation ends when the human leaves");

        let listed = screen
            .frames
            .iter()
            .find_map(|frame| match &frame.sheet {
                Some(Sheet::List { rows, .. }) => Some(rows.clone()),
                _ => None,
            })
            .expect("the listing is drawn");
        let row = listed
            .iter()
            .find(|row| row.session == elsewhere)
            .expect("the log's other session is a row");
        assert!(
            !row.held && row.goal.is_some(),
            "a session only the log holds says so, and carries the goal it recorded: {row:?}",
        );
        assert_eq!(
            listed[0].session.as_uuid().to_string(),
            SESSION,
            "the session this process holds comes first",
        );

        assert_eq!(
            outcome.left.len(),
            2,
            "the session the human switched to was opened by this process",
        );
        let adopted = outcome
            .left
            .iter()
            .find(|left| left.session == elsewhere)
            .expect("the adopted session is one of the two");
        assert!(
            adopted
                .lines
                .contains(&Line::Human("the older line".to_owned())),
            "the adopted session's own conversation is on screen: {:?}",
            adopted.lines,
        );
        assert_eq!(
            adopted.ending, None,
            "switching to a session does not end it",
        );
    }
}
