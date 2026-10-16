//! The built binary, run the way a shell runs it: no terminal on either end, so it refuses.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    const SESSION: &str = "018f2f1e-0000-7000-8000-000000000000";

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
}
