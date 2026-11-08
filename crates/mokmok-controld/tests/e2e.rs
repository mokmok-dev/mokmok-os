//! The daemon served over a real socket.

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use serde_json::Value;
    use tempfile::TempDir;

    struct Daemon {
        child: Child,
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Daemon {
        fn spawn(dir: &TempDir) -> (Self, SocketAddr) {
            let child = Command::new(env!("CARGO_BIN_EXE_mokmok-controld"))
                .arg("--scripted")
                .arg("--bind")
                .arg("127.0.0.1:0")
                .arg("--data-dir")
                .arg(dir.path())
                .arg("--workers")
                .arg("1")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("the daemon starts");
            let mut daemon = Self { child };
            let stdout = daemon.child.stdout.take().expect("the daemon has stdout");
            let (lines, received) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if lines.send(line).is_err() {
                        return;
                    }
                }
            });

            let deadline = Instant::now() + Duration::from_secs(10);
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
    }

    fn request(
        address: SocketAddr,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(address).expect("the daemon listens");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
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

    #[test]
    fn the_daemon_serves_a_scripted_task_end_to_end() {
        let dir = TempDir::new().expect("a temp dir");
        let (_daemon, address) = Daemon::spawn(&dir);

        let (status, body) = request(address, "GET", "/", None);
        assert_eq!(status, 200);
        assert!(body.contains("mokmok control plane"));

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

        let deadline = Instant::now() + Duration::from_secs(2);
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

        let updates = detail["state"]["updates"]
            .as_array()
            .expect("the transcript is a list");
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0]["text"], "the scripted agent received the task");
        assert_eq!(updates[1]["title"], "inspect the workspace");
        assert_eq!(detail["state"]["outcome"], "the scripted run completed");
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
            .set_read_timeout(Some(Duration::from_secs(2)))
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

        let pid = daemon.child.id().to_string();
        let sent = Command::new("kill")
            .args(["-INT", &pid])
            .status()
            .expect("kill runs");
        assert!(sent.success(), "SIGINT reached the daemon");

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match daemon.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {},
                Err(error) => panic!("cannot wait for the daemon: {error}"),
            }
            if Instant::now() >= deadline {
                let _ = daemon.child.kill();
                panic!("the daemon ignored SIGINT while /api/events was open");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
