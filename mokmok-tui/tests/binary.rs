//! The built binary, run the way a shell runs it: on pipes, where it refuses, and on a
//! pseudo-terminal, where it draws.

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write as _;
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
            shell().display().to_string(),
            "--allow-tool".to_owned(),
            shell().display().to_string(),
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

    /// The real adapter, reached the only way a test can reach it: as a child with the pty's slave
    /// on its own stdin and stdout. In this process the adapter is out of reach, because
    /// `Terminal::open` probes the process's own standard input and output, and a test harness
    /// cannot put a terminal there.
    #[test]
    fn a_conversation_on_a_terminal_draws_a_frame_and_hands_the_shell_back() {
        let scratch = Scratch::new("pty");
        let script = scratch.0.join("script.json");
        std::fs::write(&script, b"[]").expect("the script is written");

        // The 80x10 screen the asserted moves belong to.
        let size = Winsize {
            ws_row: 10,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&size), None).expect("a pseudo-terminal is opened");
        // This process keeps a slave of its own: with every slave closed the tty hangs up, and the
        // settings read back at the end would be a fresh terminal's rather than this child's.
        let kept = pty.slave.try_clone().expect("the slave is kept");
        let input = pty
            .slave
            .try_clone()
            .expect("the slave is cloned for stdin");
        let mut master = File::from(pty.master);
        let flags = fcntl(&master, FcntlArg::F_GETFL).expect("the master's flags are read");
        fcntl(
            &master,
            FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
        )
        .expect("the master is made non-blocking");

        let mut child = Reaped::new(
            Command::new(env!("CARGO_BIN_EXE_mokmok-tui"))
                .args(args(&scratch.0, &script))
                .stdin(Stdio::from(input))
                .stdout(Stdio::from(pty.slave))
                .stderr(Stdio::piped())
                .spawn()
                .expect("the binary runs"),
        );

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
}
