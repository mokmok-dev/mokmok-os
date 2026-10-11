mod common;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use crate::common::tests::{Scratch, config, issued, shell, within};
    use mokmok_sandbox::{LimitsOnly, Outcome, Sandbox};

    #[tokio::test]
    async fn limits_only_does_not_restrict_reads() {
        let scratch = Scratch::new("unconfined-read");
        let sentinel = scratch.write("outside/sentinel.txt", "top-secret\n");
        let sandbox = LimitsOnly::new(config(&scratch));
        let request = shell(&format!("cat {}", sentinel.display()));

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.outcome(), Outcome::Exited(0));
        assert_eq!(completed.stdout().bytes(), b"top-secret\n");
        completed.cleanup().unwrap();
    }
}
