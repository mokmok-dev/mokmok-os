mod common;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use crate::common::tests::{Scratch, config, issued, shell, which, within};
    use mokmok_sandbox::{Confined, Outcome, Sandbox};
    use std::net::TcpListener;

    fn unconfined(script: &str) -> std::process::Output {
        std::process::Command::new(which("sh"))
            .arg("-c")
            .arg(script)
            .output()
            .expect("the unconfined shell could not be started")
    }

    #[tokio::test]
    async fn a_confined_workload_cannot_reach_a_tcp_peer() {
        let scratch = Scratch::new("no-tcp");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|error| {
            panic!("no loopback listener to prove the difference against: {error}")
        });
        let port = listener.local_addr().unwrap().port();
        let script = format!("echo > /dev/tcp/127.0.0.1/{port}");

        let control = unconfined(&script);
        assert!(
            control.status.success(),
            "the unconfined shell could not connect to the listener on 127.0.0.1:{port}, so this \
             environment cannot show the difference: {}",
            String::from_utf8_lossy(&control.stderr),
        );

        let sandbox = Confined::new(config(&scratch)).unwrap();
        let run = sandbox.spawn(&shell(&script), &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();
        let stderr = completed.stderr().text();

        assert_ne!(
            completed.outcome(),
            Outcome::Exited(0),
            "the confined shell connected to 127.0.0.1:{port}: {}",
            completed.stdout().text(),
        );
        assert!(
            stderr.contains("Permission denied"),
            "the connect failed without the kernel refusing it: {stderr}",
        );
        // The peer is listening, so `Connection refused` would mean the attempt reached it.
        assert!(
            !stderr.contains("Connection refused"),
            "the listen socket refused the connect, so the kernel did not: {stderr}",
        );
        completed.cleanup().unwrap();
    }
}
