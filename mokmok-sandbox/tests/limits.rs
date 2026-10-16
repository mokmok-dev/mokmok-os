mod common;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use crate::common::tests::{BOUND, Scratch, config, issued, shell, within};
    use mokmok_sandbox::{
        Access, Capability, Completed, Confined, Exposure, LimitsOnly, Outcome, Request, Sandbox,
    };
    use std::num::NonZeroU64;
    use std::time::Instant;

    /// `ulimit -v` prints KiB and `ulimit -f` prints 512-byte blocks, so the unit conversions show
    /// up here rather than being asserted by construction.
    fn expect_reported_limits(completed: &Completed) {
        assert_eq!(completed.stdout().text(), "65536\n64\n2048\n");
        assert_eq!(completed.outcome(), Outcome::Exited(0));
    }

    fn all_three() -> Request {
        let mut request = shell("ulimit -v; ulimit -n; ulimit -f");
        request.limits.address_space = NonZeroU64::new(64 * 1024 * 1024);
        request.limits.open_files = NonZeroU64::new(64);
        request.limits.file_size = NonZeroU64::new(1024 * 1024);

        request
    }

    #[tokio::test]
    async fn cpu_time_is_capped() {
        let scratch = Scratch::new("cpu");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell("while :; do :; done");
        request.limits.cpu = NonZeroU64::new(1);

        let started = Instant::now();
        let run = sandbox.spawn(&request, &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert!(
            started.elapsed() < BOUND,
            "the cpu limit did not end the workload: {:?}",
            completed.outcome(),
        );
        // The soft limit equals the hard limit, so the kernel's kill threshold is reached in the
        // same check and the signal is `SIGKILL` rather than `SIGXCPU`.
        assert_eq!(completed.outcome(), Outcome::Signalled(9));
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn a_file_size_limit_stops_a_write() {
        let scratch = Scratch::new("file-size");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell(
            "i=0; while [ $i -lt 5000 ]; do i=$((i+1)); printf '0123456789' >> big.txt; done",
        );
        request.limits.file_size = NonZeroU64::new(4096);

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let workspace = run.workspace().to_path_buf();
        let completed = within("wait", run.wait()).await.unwrap();
        let written = std::fs::metadata(workspace.join("big.txt")).unwrap().len();

        assert!(
            written <= 4096,
            "the file reached {written} bytes past its 4096 byte limit",
        );
        assert_eq!(completed.outcome(), Outcome::Signalled(25));
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn the_workload_reports_the_limits_it_was_given() {
        let scratch = Scratch::new("limits-confined");
        let sandbox = Confined::new(config(&scratch)).unwrap();

        let run = sandbox.spawn(&all_three(), &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        expect_reported_limits(&completed);
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn the_default_capture_cap_is_one_mebibyte() {
        let scratch = Scratch::new("default-cap");
        let long = scratch.write("long.txt", &"a".repeat(1_100_000));
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell(&format!("cat {}", long.display()));
        request.exposures.push(Exposure::read(&long));
        let mut capabilities = issued();
        capabilities.push(Capability::issued(&long, Access::READ));

        let run = sandbox.spawn(&request, &capabilities).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.stdout().bytes().len(), 1_048_576);
        assert!(completed.stdout().truncated());
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn the_degraded_backend_reports_the_same_limits() {
        let scratch = Scratch::new("limits-only");
        let sandbox = LimitsOnly::new(config(&scratch));

        let run = sandbox.spawn(&all_three(), &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        expect_reported_limits(&completed);
        completed.cleanup().unwrap();
    }
}
