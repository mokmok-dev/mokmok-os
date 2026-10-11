mod common;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use crate::common::tests::{Scratch, config, issued, program, shell, which, within};
    use mokmok_sandbox::{Access, Capability, Confined, Exposure, Outcome, Sandbox};

    #[tokio::test]
    async fn a_read_outside_the_exposures_is_denied() {
        let scratch = Scratch::new("read-denied");
        let sentinel = scratch.write("outside/sentinel.txt", "top-secret\n");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let request = shell(&format!("cat {}", sentinel.display()));

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.outcome(), Outcome::Exited(1));
        assert!(
            !completed.stdout().text().contains("top-secret"),
            "the sentinel's bytes reached the caller: {}",
            completed.stdout().text(),
        );
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn a_granted_read_succeeds() {
        let scratch = Scratch::new("read-granted");
        let sentinel = scratch.write("granted/sentinel.txt", "top-secret\n");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell(&format!("cat {}", sentinel.display()));
        request.exposures.push(Exposure::read(&sentinel));
        let mut capabilities = issued();
        capabilities.push(Capability::issued(&sentinel, Access::READ));

        let run = sandbox.spawn(&request, &capabilities).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.outcome(), Outcome::Exited(0));
        assert_eq!(completed.stdout().bytes(), b"top-secret\n");
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn a_write_outside_the_workspace_is_denied() {
        let scratch = Scratch::new("write-denied");
        // The directory exists, so the refusal is the ruleset's and not a missing parent.
        std::fs::create_dir_all(scratch.path().join("outside")).unwrap();
        let outside = scratch.path().join("outside/written.txt");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let request = shell(&format!("echo nope > {}", outside.display()));

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert!(
            !outside.exists(),
            "the workload wrote outside its workspace"
        );
        assert_ne!(completed.outcome(), Outcome::Exited(0));
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn a_write_inside_the_workspace_succeeds() {
        let scratch = Scratch::new("write-workspace");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let request = shell("printf written > out.txt");

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let workspace = run.workspace().to_path_buf();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.outcome(), Outcome::Exited(0));
        assert_eq!(completed.workspace(), workspace.as_path());
        assert_eq!(
            std::fs::read(workspace.join("out.txt")).unwrap(),
            b"written"
        );
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn the_workloads_own_file_is_exposed() {
        let scratch = Scratch::new("own-file");
        let sibling = scratch.write("bin/sibling.txt", "sibling-secret\n");
        let script = scratch.script(
            "bin/script.sh",
            &format!(
                "#!{}\necho ran\ncat {}\n",
                which("sh").display(),
                sibling.display()
            ),
        );
        let sandbox = Confined::new(config(&scratch)).unwrap();

        // The script is outside every grant: the plan is what exposes it, so that it can run at
        // all, and the sibling beside it stays unreadable.
        let run = sandbox.spawn(&program(&script), &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.stdout().bytes(), b"ran\n");
        assert!(
            !completed.stdout().text().contains("sibling-secret"),
            "a sibling of the program was readable: {}",
            completed.stdout().text(),
        );
        assert_eq!(completed.outcome(), Outcome::Exited(1));
        completed.cleanup().unwrap();
    }
}
