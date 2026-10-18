//! The built binary, run the way a shell runs it: on pipes, where it refuses, and on a
//! pseudo-terminal, where it draws.

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Read as _;
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::os::fd::AsFd;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::time::{Duration, Instant};

    use nix::errno::Errno;
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use nix::pty::{Winsize, openpty};
    use nix::sys::termios::{LocalFlags, tcgetattr};
    use nix::unistd::read;

    const SESSION: &str = "018f2f1e-0000-7000-8000-000000000000";

    /// How long the child is given to draw a frame, or to exit once it is told to. Kept well inside
    /// the twenty-second kill `cargo mutants` sets by default, so a mutated adapter fails this case
    /// instead of hanging into a timeout, which the gate scores against the suite as well.
    const PATIENCE: Duration = Duration::from_secs(5);

    /// How long to wait between readings of the master.
    const POLL: Duration = Duration::from_millis(5);

    /// What the adapter puts on the master for the first frame of an 80x10 screen, before any key:
    /// the alternate screen, the heading on row 1, the empty input line on row 9, the status on row
    /// 10, and the cursor one column past the `> ` the line starts with.
    ///
    /// Each run of words arrives behind its own move rather than as one line, because the backend
    /// writes only the cells a screen of spaces did not already hold.
    const FRAME: &[&[u8]] = &[
        b"\x1b[?1049h",
        b"\x1b[1;1Hmokmok-tui",
        b"\x1b[1;13H018f2f1e-0000-7000-8000-000000000000",
        b"\x1b[1;51Hyour\x1b[1;56Hturn",
        b"\x1b[9;1H>",
        b"\x1b[10;1Hyour\x1b[10;6Hturn",
        b"\x1b[9;3H",
    ];

    /// The same screen once `hi` is typed: the first character on the input row, and the cursor two
    /// columns further along, past the whole line.
    const TYPED: &[&[u8]] = &[b"\x1b[9;3Hh", b"\x1b[9;5H"];

    const LEAVE_ALTERNATE: &[u8] = b"\x1b[?1049l";
    const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

    /// The interrupt a human types to leave, which the child's raw mode delivers as this one byte.
    const CTRL_C: &[u8] = b"\x03";

    /// A directory of this case's own, removed when it ends.
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
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The first shell on `PATH`, which stands in for the confiner and the one allowed tool: the
    /// run fails before anything is dispatched.
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

        panic!("no shell on PATH");
    }

    /// The sandbox's confiner, which is what actually runs a command. A package-scoped build
    /// leaves it out, because it belongs to another package, so a case that dispatches a command
    /// needs the workspace built first.
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

    /// The flags the configuration resolves for a scripted run.
    fn args(
        scratch: &Path,
        script: &Path,
    ) -> Vec<String> {
        let checkout = scratch.join("checkout");

        vec![
            "--data-dir".to_owned(),
            scratch.join("data").display().to_string(),
            "--confiner".to_owned(),
            confiner().display().to_string(),
            "--allow-tool".to_owned(),
            shell().display().to_string(),
            "--allow-path".to_owned(),
            "/nix/store".to_owned(),
            "--allow-write".to_owned(),
            checkout.display().to_string(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--goal".to_owned(),
            "hold a conversation".to_owned(),
            "--checkout".to_owned(),
            checkout.display().to_string(),
            "--script".to_owned(),
            script.display().to_string(),
        ]
    }

    /// Everything the master has to say right now, waiting for none of it.
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

    /// Where `needle` sits in `hay` at or after `from`, when it sits there at all.
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

    /// Waits, taking what it reads, until every one of `wanted` is on the master.
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

    /// Waits for the child to exit, taking everything it says on the master before it does.
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
                "the child had not exited within {PATIENCE:?}: {}",
                transcript.escape_ascii(),
            );
            std::thread::sleep(POLL);
        }
    }

    /// The conversation process, reaped when this value drops. A mutated adapter fails an assertion
    /// in the middle of the pty case, and without this the child and its pseudo-terminal outlive the
    /// test: one leaked per mutant, for every run of the gate.
    struct Reaped(Child);

    impl Reaped {
        fn new(child: Child) -> Self {
            Self(child)
        }

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

    #[test]
    fn a_run_with_no_terminal_fails_and_says_why() {
        let scratch = Scratch::new("binary");
        let script = scratch.0.join("script.json");
        std::fs::write(&script, b"[]").expect("the script is written");

        let run = Command::new(env!("CARGO_BIN_EXE_mokmok-tui"))
            .env("XDG_CONFIG_HOME", scratch.0.join("config"))
            .args(args(&scratch.0, &script))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("the binary runs");
        let stderr = String::from_utf8(run.stderr).expect("the message is text");

        assert_eq!(
            run.status.code(),
            Some(1),
            "a run that cannot draw a screen must fail: {stderr}",
        );
        assert!(
            stderr.starts_with("mokmok-tui: this is not a terminal"),
            "the failure names what is missing: {stderr}",
        );
    }

    const CTRL_N: &[u8] = b"\x0e";
    const CTRL_O: &[u8] = b"\x0f";

    /// The real adapter, reached the only way a test can reach it: as a child with the pty's slave
    /// on its own stdin and stdout. In this process the adapter is out of reach, because
    /// `Terminal::open` probes the process's own standard input and output, and a test harness
    /// cannot put a terminal there.
    ///
    /// The child, the master made non-blocking, and a slave this process keeps: with every slave
    /// closed the tty hangs up, and the settings read back at the end would be a fresh terminal's.
    fn on_a_pty(
        scratch: &Path,
        arguments: Vec<String>,
    ) -> (Reaped, File, File) {
        // The 80x10 screen the asserted moves belong to.
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

        let child = Reaped::new(
            Command::new(env!("CARGO_BIN_EXE_mokmok-tui"))
                .env("XDG_CONFIG_HOME", scratch.join("config"))
                .env("MOKMOK_TEST_KEY", "test-key")
                .args(arguments)
                .stdin(Stdio::from(input))
                .stdout(Stdio::from(pty.slave))
                .stderr(Stdio::piped())
                .spawn()
                .expect("the binary runs"),
        );

        (child, master, File::from(kept))
    }

    #[test]
    fn a_conversation_on_a_terminal_draws_a_frame_and_hands_the_shell_back() {
        let scratch = Scratch::new("pty");
        let script = scratch.0.join("script.json");
        std::fs::write(&script, b"[]").expect("the script is written");

        let (mut child, mut master, kept) = on_a_pty(&scratch.0, args(&scratch.0, &script));

        let deadline = Instant::now() + PATIENCE;
        let mut transcript = Vec::new();
        assert!(
            await_bytes(&master, &mut transcript, FRAME, deadline),
            "the frame is on the master before the human interrupts: {}",
            transcript.escape_ascii(),
        );
        assert!(
            child
                .try_wait()
                .expect("the child is asked about")
                .is_none(),
            "the frame arrives from a conversation that is still running: {}",
            transcript.escape_ascii(),
        );

        master.write_all(b"hi").expect("two characters are typed");
        assert!(
            await_bytes(&master, &mut transcript, TYPED, deadline),
            "the typed line is drawn and the cursor follows it: {}",
            transcript.escape_ascii(),
        );

        // The state the exit has to undo, read from the same slave: without it, the flags at the
        // end could be the ones the terminal started with and prove nothing about the child.
        let raw = tcgetattr(&kept)
            .expect("the slave's settings are read")
            .local_flags;
        assert!(
            !raw.contains(LocalFlags::ICANON | LocalFlags::ECHO),
            "the conversation's own screen is raw: {raw:?}",
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
                "the child leaves the alternate screen as it goes: {}",
                transcript.escape_ascii(),
            )
        });
        assert!(
            found(&transcript, SHOW_CURSOR, leaving).is_some(),
            "the cursor comes back after the alternate screen is left: {}",
            transcript.escape_ascii(),
        );

        let settings = tcgetattr(&kept)
            .expect("the slave's settings are read")
            .local_flags;
        assert!(
            settings.contains(LocalFlags::ICANON),
            "canonical input is back for the shell: {settings:?}",
        );
        assert!(
            settings.contains(LocalFlags::ECHO),
            "echo is back for the shell: {settings:?}",
        );
    }

    /// A session whose turn the model holds is drawn before the human's next key is read, which is
    /// the state in which the adapter has to ask the terminal what it holds instead of waiting on it.
    #[test]
    fn a_working_session_is_drawn_and_the_listing_opens_on_a_real_terminal() {
        let scratch = Scratch::new("pty-sessions");
        let script = scratch.0.join("script.json");
        std::fs::write(&script, br#"[{"action": "final", "say": "done"}]"#)
            .expect("the script is written");

        let (mut child, mut master, _kept) = on_a_pty(&scratch.0, args(&scratch.0, &script));
        let deadline = Instant::now() + PATIENCE;
        let mut transcript = Vec::new();

        // A key typed before the child has the terminal in raw mode is read by the line discipline,
        // so the first frame is what says the screen is the conversation's own.
        assert!(
            await_bytes(&master, &mut transcript, &[b"mokmok-tui"], deadline),
            "the conversation's own screen is up: {}",
            transcript.escape_ascii(),
        );

        // One write, so the key after the line is already in the terminal's queue while the turn is
        // in flight: the loop has to ask what the terminal holds rather than wait on it, or the key
        // is read only once nothing can move and the answer overtakes it. The carriage return is the
        // Enter a terminal in raw mode delivers.
        master
            .write_all(b"hi\rz")
            .expect("a line, and one more key");
        assert!(
            await_bytes(
                &master,
                &mut transcript,
                &[b"\x1b[9;3Hz", b"done"],
                deadline
            ),
            "the key and the answer are both drawn: {}",
            transcript.escape_ascii(),
        );

        let typed_at = found(&transcript, b"\x1b[9;3Hz", 0).expect("the key is drawn");
        let answered_at = found(&transcript, b"done", 0).expect("the answer is drawn");
        assert!(
            typed_at < answered_at,
            "a key pressed while the turn is in flight is answered before the next step: {}",
            transcript.escape_ascii(),
        );
        assert!(
            await_bytes(
                &master,
                &mut transcript,
                &[b"the agent", b"working"],
                deadline
            ),
            "a frame drawn while the turn is in flight: {}",
            transcript.escape_ascii(),
        );

        master.write_all(CTRL_O).expect("the listing is asked for");
        assert!(
            await_bytes(
                &master,
                &mut transcript,
                // The backend writes only the cells that changed, so the status row arrives as its
                // words rather than as one run.
                &[b"held;", b"opens", b"closes"],
                deadline,
            ),
            "the listing is drawn on the real terminal: {}",
            transcript.escape_ascii(),
        );

        master
            .write_all(CTRL_O)
            .expect("the same key closes the listing");
        master
            .write_all(CTRL_N)
            .expect("a new session is asked for");
        assert!(
            await_bytes(&master, &mut transcript, &[b"goal>"], deadline),
            "the prompt that names a goal is drawn: {}",
            transcript.escape_ascii(),
        );

        master.write_all(CTRL_C).expect("the interrupt is typed");
        let status = await_exit(&mut child, &master, &mut transcript, deadline);
        assert_eq!(
            status.code(),
            Some(0),
            "the conversation leaves when the human does: {}",
            transcript.escape_ascii(),
        );
        assert!(
            found(&transcript, b"--- session", 0).is_some(),
            "the plain replay names the session it hands back: {}",
            transcript.escape_ascii(),
        );
    }

    fn stream_args(
        scratch: &Path,
        port: u16,
    ) -> Vec<String> {
        let checkout = scratch.join("checkout");

        vec![
            "--data-dir".to_owned(),
            scratch.join("data").display().to_string(),
            "--confiner".to_owned(),
            confiner().display().to_string(),
            "--allow-tool".to_owned(),
            shell().display().to_string(),
            "--allow-path".to_owned(),
            "/nix/store".to_owned(),
            "--allow-write".to_owned(),
            checkout.display().to_string(),
            "--session".to_owned(),
            SESSION.to_owned(),
            "--goal".to_owned(),
            "hold a conversation".to_owned(),
            "--checkout".to_owned(),
            checkout.display().to_string(),
            "--provider".to_owned(),
            "ollama".to_owned(),
            "--model".to_owned(),
            "m".to_owned(),
            "--endpoint".to_owned(),
            format!("http://127.0.0.1:{port}/v1"),
            "--env-key".to_owned(),
            "MOKMOK_TEST_KEY".to_owned(),
        ]
    }

    /// The hold is what makes the assertion about the first fragment mean something: the second
    /// fragment does not exist on the wire while the first is being drawn.
    fn serve_stream(release: std::sync::mpsc::Receiver<()>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the mock endpoint binds");
        let port = listener.local_addr().expect("the port").port();

        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let mut buffer = [0_u8; 4096];
            while stream.read(&mut buffer).is_ok_and(|read| read > 0) {}

            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            );
            let _ = stream.write_all(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"stream-one\"},\"finish_reason\":null}]}\n\n",
            );
            let _ = stream.flush();

            let _ = release.recv_timeout(Duration::from_secs(10));

            let _ = stream.write_all(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"stream-two\"},\"finish_reason\":\"stop\"}]}\n\n",
            );
            let _ = stream.write_all(b"data: [DONE]\n\n");
            let _ = stream.flush();
        });

        port
    }

    #[test]
    fn a_key_typed_while_a_command_runs_is_drawn_before_the_answer() {
        // The confiner belongs to another package. A package-scoped build, which is what the
        // mutation gate makes, leaves it out, and a shell standing in for it refuses every
        // command. The unit case over the worker covers the same screen behaviour without one.
        if !confiner()
            .to_string_lossy()
            .ends_with("mokmok-sandbox-confine")
        {
            return;
        }

        let scratch = Scratch::new("pty-command");
        let script = scratch.0.join("script.json");
        std::fs::write(
            &script,
            br#"[{"action":"command","command":"sleep 2"},{"action":"final","say":"done"}]"#,
        )
        .expect("the script is written");

        let (mut child, mut master, _kept) = on_a_pty(&scratch.0, args(&scratch.0, &script));
        let deadline = Instant::now() + PATIENCE;
        let mut transcript = Vec::new();

        assert!(
            await_bytes(&master, &mut transcript, &[b"mokmok-tui"], deadline),
            "the conversation's own screen is up: {}",
            transcript.escape_ascii(),
        );

        master.write_all(b"hi\r").expect("a line is submitted");
        std::thread::sleep(Duration::from_millis(500));
        master
            .write_all(b"z")
            .expect("a key is typed while the command runs");
        assert!(
            await_bytes(&master, &mut transcript, &[b"\x1b[9;3Hz"], deadline),
            "a key typed during the command is echoed: {}",
            transcript.escape_ascii(),
        );

        assert!(
            await_bytes(&master, &mut transcript, &[b"done"], deadline),
            "the command's answer follows: {}",
            transcript.escape_ascii(),
        );

        let typed_at = found(&transcript, b"\x1b[9;3Hz", 0).expect("the key is drawn");
        let answered_at = found(&transcript, b"done", 0).expect("the answer is drawn");
        assert!(
            typed_at < answered_at,
            "the key is drawn while the command still runs: {}",
            transcript.escape_ascii(),
        );

        master.write_all(CTRL_C).expect("the interrupt is typed");
        let status = await_exit(&mut child, &master, &mut transcript, deadline);
        assert_eq!(
            status.code(),
            Some(0),
            "the conversation leaves when the human does: {}",
            transcript.escape_ascii(),
        );
    }

    #[test]
    fn a_streamed_answer_is_drawn_while_it_arrives_and_keys_still_echo() {
        let scratch = Scratch::new("pty-stream");
        let (release, hold) = std::sync::mpsc::channel();
        let port = serve_stream(hold);
        let (mut child, mut master, _kept) = on_a_pty(&scratch.0, stream_args(&scratch.0, port));
        let deadline = Instant::now() + PATIENCE;
        let mut transcript = Vec::new();

        assert!(
            await_bytes(&master, &mut transcript, &[b"mokmok-tui"], deadline),
            "the conversation's own screen is up: {}",
            transcript.escape_ascii(),
        );

        master.write_all(b"hi\r").expect("a line is submitted");
        assert!(
            await_bytes(&master, &mut transcript, &[b"stream-one"], deadline),
            "the first fragment is drawn before the second is sent: {}",
            transcript.escape_ascii(),
        );

        master
            .write_all(b"z")
            .expect("a key is typed while the call runs");
        assert!(
            await_bytes(&master, &mut transcript, &[b"\x1b[9;3Hz"], deadline),
            "a key typed during the stream is echoed: {}",
            transcript.escape_ascii(),
        );

        release.send(()).expect("the stream is released");
        assert!(
            await_bytes(&master, &mut transcript, &[b"stream-two"], deadline),
            "the rest of the answer is drawn: {}",
            transcript.escape_ascii(),
        );

        let first = found(&transcript, b"stream-one", 0).expect("the first fragment is drawn");
        let second = found(&transcript, b"stream-two", 0).expect("the second fragment is drawn");
        assert!(
            first < second,
            "the screen follows the stream in order: {}",
            transcript.escape_ascii(),
        );

        master.write_all(CTRL_C).expect("the interrupt is typed");
        let status = await_exit(&mut child, &master, &mut transcript, deadline);
        assert_eq!(
            status.code(),
            Some(0),
            "the conversation leaves when the human does: {}",
            transcript.escape_ascii(),
        );
    }
}
