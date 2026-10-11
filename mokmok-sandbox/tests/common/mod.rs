// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code. A helper only
// some integration test targets reach would otherwise be dead code in the others.
#[cfg(test)]
#[allow(dead_code)]
pub mod tests {
    use mokmok_protocol::ExecutionID;
    use mokmok_sandbox::{Access, Capability, Config, Exposure, Request};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use uuid::timestamp::{Timestamp, context::NoContext};

    /// Long enough for a healthy execution to answer, short enough that a stuck one fails the
    /// suite instead of hanging it.
    pub const BOUND: Duration = Duration::from_secs(10);

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    /// A directory of this test's own, removed when the test ends.
    pub struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        pub fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "mokmok-sandbox-{}-{}-{name}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));

            Self { path }
        }

        pub fn path(&self) -> &Path {
            &self.path
        }

        /// The sandbox root, never created by the test: nothing must be left behind without an
        /// execution.
        pub fn root(&self) -> PathBuf {
            self.path.join("root")
        }

        pub fn write(
            &self,
            name: &str,
            body: &str,
        ) -> PathBuf {
            let path = self.path.join(name);

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, body).unwrap();

            path
        }

        /// A file with an execute bit, for a workload that is run directly.
        pub fn script(
            &self,
            name: &str,
            body: &str,
        ) -> PathBuf {
            let path = self.write(name, body);
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
            std::fs::set_permissions(&path, permissions).unwrap();

            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    pub fn config(scratch: &Scratch) -> Config {
        Config {
            root: scratch.root(),
            confiner: PathBuf::from(env!("CARGO_BIN_EXE_mokmok-sandbox-confine")),
            ..Config::default()
        }
    }

    pub fn execution() -> ExecutionID {
        ExecutionID::new(Timestamp::now(NoContext))
    }

    pub fn which(name: &str) -> PathBuf {
        let path = std::env::var_os("PATH").unwrap_or_default();

        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(name);

            if candidate.is_file() {
                return candidate;
            }
        }

        panic!("{name} is not on PATH");
    }

    pub fn issued() -> Vec<Capability> {
        vec![
            Capability::issued("/nix/store", Access::READ_EXECUTE),
            Capability::issued("/dev/null", Access::READ_WRITE),
        ]
    }

    pub fn exposures() -> Vec<Exposure> {
        vec![Exposure::read_execute("/nix/store")]
    }

    pub fn shell(script: &str) -> Request {
        let mut request = Request::new(execution(), which("sh"));
        request.args = vec!["-c".into(), script.into()];
        request.exposures = exposures();

        request
    }

    pub fn program(path: &Path) -> Request {
        let mut request = Request::new(execution(), path);
        request.exposures = exposures();

        request
    }

    pub async fn within<T>(
        what: &str,
        future: impl std::future::Future<Output = T>,
    ) -> T {
        match tokio::time::timeout(BOUND, future).await {
            Ok(value) => value,
            Err(elapsed) => panic!("{what} did not answer within {BOUND:?}: {elapsed}"),
        }
    }

    /// True while the pid is a running process. A zombie the supervisor has not reaped yet is not
    /// a running process, and `/proc` is the only thing that can tell the two apart.
    pub fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| !matches!(state(&stat), 'Z' | 'X'))
    }

    /// The state field of `/proc/<pid>/stat`. The command name can hold a `)`, so the fields after
    /// the last one are what this reads.
    fn state(stat: &str) -> char {
        stat.rsplit(')')
            .next()
            .and_then(|rest| rest.trim_start().chars().next())
            .unwrap_or('X')
    }

    pub async fn await_death(pid: u32) -> bool {
        for _ in 0..300 {
            if !alive(pid) {
                return true;
            }

            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        false
    }

    pub fn nothing_left(root: &Path) -> bool {
        root.read_dir()
            .map_or(true, |mut entries| entries.next().is_none())
    }
}
