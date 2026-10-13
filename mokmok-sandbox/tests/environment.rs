//! The environment a confined workload inherits, driven against a real spawned workload.
//!
//! `Environment::Extend` is the arm that keeps whatever the spawning process holds. Proving it needs
//! a marker that is really in that process's environment, and a test cannot set its own safely, so
//! the test re-runs itself under `Command::env` once. The arm that clears the environment is proven
//! the other way round in `mokmok-kernel/tests/environment.rs`: there a daemon that holds the marker
//! must not hand it to its workload.

mod common;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use crate::common::tests::{Scratch, config, issued, shell, within};
    use mokmok_sandbox::{Confined, Environment, Sandbox};
    use std::process::Command;

    const MARKER: &str = "MOKMOK_INHERITED_MARKER";
    const WORKER: &str = "the_extend_arm_passes_an_inherited_marker_through";

    #[test]
    fn the_extend_arm_passes_an_inherited_marker_through() {
        if std::env::var_os(MARKER).is_none() {
            let rerun = Command::new(std::env::current_exe().unwrap())
                .env(MARKER, "present")
                .arg(WORKER)
                .output()
                .unwrap();
            let out = String::from_utf8_lossy(&rerun.stdout);

            assert!(
                rerun.status.success() && out.contains("1 passed"),
                "the rerun that holds the marker did not pass:\n{out}\n{}",
                String::from_utf8_lossy(&rerun.stderr),
            );

            return;
        }

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let scratch = Scratch::new("extend-marker");
                let sandbox = Confined::new(config(&scratch)).unwrap();
                let mut request = shell(&format!("printf '%s' \"${{{MARKER}}}\""));
                request.environment = Environment::Extend(Vec::new());

                let run = sandbox.spawn(&request, &issued()).unwrap();
                let completed = within("wait", run.wait()).await.unwrap();

                assert_eq!(completed.stdout().text(), "present");
                completed.cleanup().unwrap();
            });
    }
}
