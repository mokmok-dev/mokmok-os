//! The environment a confined workload is handed, driven the way a caller drives it: through
//! `mokmokd`, against a real daemon and a real confined process.
//!
//! `mokmokd` inherits a marker variable from the process that starts it, and the tool it runs must
//! not see it: the tool path clears the inherited environment and sets `PATH` plus the operator's
//! `--tool-env` entries. The assertion is on the bytes the workload captured, read back out of the
//! artifact store, so it fails if the daemon passes anything through.
//!
//! The harness lives inside a `#[cfg(test)]` module because that is what tells the workspace's
//! clippy settings that this is test code, where `unwrap` and `panic!` are allowed.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use mokmok_artifact::{BlobStore, ContentHash};

    const SESSION: &str = "018f2f1e-0000-7000-8000-000000000000";
    const KEY: &str = "018f2f1e-0000-7000-8000-0000000000e5";
    const MARKER: &str = "MOKMOK_INHERITED_MARKER";
    const TOOL: &str = "MOKMOK_TOOL_VAR";

    /// A directory of this test's own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mokmok-env-{name}-{}", std::process::id()));
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
        ok: bool,
        out: String,
        err: String,
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

        panic!("no shell on PATH, so the daemon cannot run a tool");
    }

    /// The directories the workload may read and execute in: the store every nix-provided binary and
    /// library lives in, and the system directories a binary built elsewhere needs.
    fn reachable() -> Vec<PathBuf> {
        let store = PathBuf::from("/nix/store");
        assert!(
            store.is_dir(),
            "this workspace builds against a nix store, and the workload runs a binary out of it",
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

    /// The bytes the `field` of the captured standard output names, read back out of the store.
    fn field(
        text: &str,
        key: &str,
    ) -> Option<String> {
        text.split_whitespace()
            .find_map(|word| word.strip_prefix(&format!("{key}=")).map(str::to_owned))
    }

    #[test]
    fn a_tool_sees_the_operators_environment_and_nothing_of_the_daemons() {
        let scratch = Scratch::new("exact");
        let script = concat!(
            r#"printf 'marker=%s path=%s extra=%s'"#,
            r#" "${MOKMOK_INHERITED_MARKER:-absent}" "$(command -v sh || echo none)""#,
            r#" "${MOKMOK_TOOL_VAR:-unset}""#,
        );
        let mut args = vec![
            "--data-dir".to_owned(),
            scratch.data().display().to_string(),
            "--confiner".to_owned(),
            confiner().display().to_string(),
            "--allow-tool".to_owned(),
            shell().display().to_string(),
            "--tool-env".to_owned(),
            format!("{TOOL}=set"),
        ];
        for path in reachable() {
            args.push("--allow-path".to_owned());
            args.push(path.display().to_string());
        }
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
            script.to_owned(),
        ]);

        // The marker is set on the daemon alone: it is what the tool path has to clear, and an
        // environment the daemon never held would make the absence below prove nothing.
        let output = Command::new(binary())
            .env(MARKER, "present")
            .args(&args)
            .output()
            .expect("the binary runs");
        let ran = Ran {
            ok: output.status.success(),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        };

        assert!(ran.ok, "{}\n{}", ran.out, ran.err);
        let hash = ran
            .out
            .lines()
            .find_map(|line| field(line, "stdout"))
            .expect("the submit reported a stdout blob");
        let store = BlobStore::open(scratch.data().join("artifacts")).unwrap();
        let bytes = store
            .read(hash.parse::<ContentHash>().unwrap())
            .expect("the captured stdout is in the store");
        let stdout = String::from_utf8(bytes).unwrap();

        assert!(
            stdout.contains("marker=absent"),
            "the daemon's own environment reached the workload: {stdout:?}",
        );
        assert!(
            stdout.contains("extra=set"),
            "the operator's --tool-env entry did not reach the workload: {stdout:?}",
        );
        let path = field(&stdout, "path").expect("the workload named the shell it found");
        assert!(
            Path::new(&path).is_file(),
            "the workload's PATH did not name a shell it could reach: {path:?}",
        );
    }
}
