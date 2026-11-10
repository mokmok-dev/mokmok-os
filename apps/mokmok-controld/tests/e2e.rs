//! The daemon served over a real socket.

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::process::{Child, ChildStderr, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use serde_json::Value;
    use tempfile::TempDir;

    struct Daemon {
        child: Child,
        stderr: Option<ChildStderr>,
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Daemon {
        fn spawn(dir: &TempDir) -> (Self, SocketAddr) {
            Self::spawn_with(dir, &[])
        }

        fn spawn_with(
            dir: &TempDir,
            extra: &[&str],
        ) -> (Self, SocketAddr) {
            let mut command = Command::new(env!("CARGO_BIN_EXE_mokmok-controld"));
            command
                .arg("--scripted")
                .arg("--bind")
                .arg("127.0.0.1:0")
                .arg("--data-dir")
                .arg(dir.path())
                .arg("--workers")
                .arg("1")
                .args(extra);
            let mut child = command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("the daemon starts");
            let stderr = child.stderr.take();
            let mut daemon = Self { child, stderr };
            let stdout = daemon.child.stdout.take().expect("the daemon has stdout");
            let (lines, received) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if lines.send(line).is_err() {
                        return;
                    }
                }
            });

            // Every wait in this file only has to stay under cargo-mutants' per-mutant
            // timeout: under `nix flake check` the machine is busy, and a real hang still
            // fails here rather than passing.
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let line = received
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("the daemon prints where it listens");
                if let Some((_, address)) = line.split_once("listening on http://") {
                    let address = address.trim().parse().expect("the line names an address");
                    return (daemon, address);
                }
            }
        }

        // The pipe ends at EOF, which only an exited child gives, so read it after a
        // `try_wait` that returned a status.
        fn stderr_text(&mut self) -> String {
            let Some(mut stderr) = self.stderr.take() else {
                return String::new();
            };
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        }
    }

    #[cfg(unix)]
    fn interrupt_and_wait(
        daemon: &mut Daemon,
        failure: &str,
    ) {
        let pid = daemon.child.id().to_string();
        let sent = Command::new("kill")
            .args(["-INT", &pid])
            .status()
            .expect("kill runs");
        assert!(sent.success(), "SIGINT reached the daemon");

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match daemon.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {},
                Err(error) => panic!("cannot wait for the daemon: {error}"),
            }
            if Instant::now() >= deadline {
                let _ = daemon.child.kill();
                panic!("{failure}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn request(
        address: SocketAddr,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(address).expect("the daemon listens");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("the socket takes a read timeout");
        let payload = body.unwrap_or("");
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
            payload.len()
        );
        stream
            .write_all(request.as_bytes())
            .expect("the request is written");
        let mut raw = String::new();
        stream
            .read_to_string(&mut raw)
            .expect("the response is read");
        parse_response(&raw)
    }

    fn parse_response(raw: &str) -> (u16, String) {
        let (head, body) = raw
            .split_once("\r\n\r\n")
            .expect("the response has a header block");
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .expect("the status line names a code");
        let length: usize = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse().ok())
            .expect("the response carries Content-Length");
        (status, body.get(..length).unwrap_or(body).to_owned())
    }

    fn submit_scripted(address: SocketAddr) -> String {
        let (status, body) = request(
            address,
            "POST",
            "/api/tasks",
            Some(
                r#"{"title":"Fix the build","instruction":"Make the tests pass.","agent":"scripted"}"#,
            ),
        );
        assert_eq!(status, 201);
        let created: Value = serde_json::from_str(&body).expect("the answer is JSON");
        created["id"]
            .as_str()
            .expect("the answer carries an id")
            .to_owned()
    }

    fn wait_for_journal_total(
        address: SocketAddr,
        id: &str,
        total: usize,
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (status, body) = request(address, "GET", &format!("/api/tasks/{id}/journal"), None);
            assert_eq!(status, 200);
            let journal: Value = serde_json::from_str(&body).expect("the journal is JSON");
            if journal["entries"].as_array().map(Vec::len) == Some(total) {
                return journal;
            }
            assert!(
                Instant::now() < deadline,
                "the journal never reached {total} entries: {body}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_journal_seq(
        address: SocketAddr,
        id: &str,
        seq: u64,
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (status, body) = request(address, "GET", &format!("/api/tasks/{id}/journal"), None);
            assert_eq!(status, 200);
            let journal: Value = serde_json::from_str(&body).expect("the journal is JSON");
            let last = journal["entries"]
                .as_array()
                .and_then(|entries| entries.last())
                .and_then(|entry| entry["seq"].as_u64());
            if last == Some(seq) {
                return journal;
            }
            assert!(
                Instant::now() < deadline,
                "the journal never reached seq {seq}: {body}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_daemon_serves_a_scripted_task_end_to_end() {
        let dir = TempDir::new().expect("a temp dir");
        let (_daemon, address) = Daemon::spawn(&dir);

        let (status, body) = request(
            address,
            "POST",
            "/api/tasks",
            Some(
                r#"{"title":"Fix the build","instruction":"Make the tests pass.","agent":"scripted"}"#,
            ),
        );
        assert_eq!(status, 201);
        let created: Value = serde_json::from_str(&body).expect("the answer is JSON");
        let id = created["id"]
            .as_str()
            .expect("the answer carries an id")
            .to_owned();

        let deadline = Instant::now() + Duration::from_secs(30);
        let detail = loop {
            let (status, body) = request(address, "GET", &format!("/api/tasks/{id}"), None);
            assert_eq!(status, 200);
            let detail: Value = serde_json::from_str(&body).expect("the detail is JSON");
            // The scripted tool call reports `"status":"completed"` too, so the task's
            // own status is the only field that can end the poll.
            if detail["state"]["status"] == "completed" {
                break detail;
            }
            assert!(
                Instant::now() < deadline,
                "the task never completed: {body}"
            );
            std::thread::sleep(Duration::from_millis(20));
        };

        assert_eq!(detail["state"]["update_count"], 0);
        assert_eq!(detail["state"]["outcome"], "the scripted run completed");

        let (status, body) = request(address, "GET", &format!("/api/tasks/{id}/journal"), None);
        assert_eq!(status, 200);
        let journal: Value = serde_json::from_str(&body).expect("the journal is JSON");
        let entries = journal["entries"]
            .as_array()
            .expect("the transcript is a list");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["text"], "the scripted agent received the task");
        assert_eq!(entries[1]["title"], "inspect the workspace");
    }

    // An `Sse` body over the daemon's own broadcast sender never ends, so this is the
    // connection the graceful shutdown used to wait for forever.
    #[cfg(unix)]
    #[test]
    fn an_open_event_stream_does_not_hold_the_daemon_open() {
        let dir = TempDir::new().expect("a temp dir");
        let (mut daemon, address) = Daemon::spawn(&dir);

        let mut stream = TcpStream::connect(address).expect("the daemon listens");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("the socket takes a read timeout");
        write!(
            stream,
            "GET /api/events HTTP/1.1\r\nHost: {address}\r\n\r\n"
        )
        .expect("the request is written");
        let mut reader = BufReader::new(stream.try_clone().expect("the socket clones"));
        let mut head = String::new();
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).expect("the headers are read");
            assert!(read > 0, "the daemon closed before the headers ended");
            if line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");

        interrupt_and_wait(
            &mut daemon,
            "the daemon ignored SIGINT while /api/events was open",
        );
    }

    // The drain bound has to start with the signal. A timeout around the whole server
    // future ended the daemon five seconds after startup instead.
    #[cfg(unix)]
    #[test]
    fn the_daemon_keeps_running_without_a_signal() {
        let dir = TempDir::new().expect("a temp dir");
        let (mut daemon, address) = Daemon::spawn(&dir);

        std::thread::sleep(Duration::from_secs(6));
        if let Some(status) = daemon.child.try_wait().expect("the daemon is waitable") {
            let stderr = daemon.stderr_text();
            panic!("the daemon exited after {status} without a signal: {stderr}");
        }
        assert_eq!(request(address, "GET", "/healthz", None).0, 200);

        interrupt_and_wait(&mut daemon, "the daemon ignored SIGINT after staying up");
    }

    #[test]
    fn the_journal_flags_shape_the_segments_on_disk() {
        let dir = TempDir::new().expect("a temp dir");
        let (_daemon, address) = Daemon::spawn_with(
            &dir,
            &["--journal-max-bytes", "1", "--journal-segments", "2"],
        );
        let id = submit_scripted(address);
        let journal = wait_for_journal_total(address, &id, 2);
        assert_eq!(journal["total"], 2);

        let journal_dir = dir.path().join("journal").join(&id);
        assert!(
            journal_dir.join("0.jsonl").exists(),
            "the first entry rotated into its own segment"
        );
        assert!(
            journal_dir.join("1.jsonl").exists(),
            "the second entry rotated into its own segment"
        );
        assert!(!journal_dir.join("2.jsonl").exists());
    }

    #[test]
    fn a_one_segment_journal_prunes_the_rotated_file() {
        let dir = TempDir::new().expect("a temp dir");
        let (_daemon, address) = Daemon::spawn_with(
            &dir,
            &["--journal-max-bytes", "1", "--journal-segments", "1"],
        );
        let id = submit_scripted(address);
        let journal = wait_for_journal_seq(address, &id, 2);
        assert_eq!(journal["total"], 1);
        assert_eq!(journal["entries"][0]["seq"], 2);

        let journal_dir = dir.path().join("journal").join(&id);
        assert!(
            journal_dir.join("1.jsonl").exists(),
            "the active segment survives"
        );
        assert!(
            !journal_dir.join("0.jsonl").exists(),
            "the rotated segment is pruned"
        );
    }
}
