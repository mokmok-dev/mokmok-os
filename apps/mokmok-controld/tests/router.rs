//! The router: assets, the task API, auth, and the live feed.

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode, header};
    use http_body_util::BodyExt;
    use mokmok_control::{
        AgentUpdate, CloudEvent, ControlEvent, EnumerableStore, JournalEvent, Runner,
        ScriptedRunner, TaskId, TaskSpec, Version,
    };
    use mokmok_controld::{AppState, ControlConfig, allowed_hosts, build_router, build_state};
    use mokmok_jobs_jsonl::JsonlEventStore;
    use mokmok_journal::{
        FileJournal, Journal, JournalConfig, JournalEntry, JournalError, JournalTail,
        MAX_RECORD_BYTES, MemoryJournal,
    };
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tower::ServiceExt;

    const SCRIPTED_TEXT: &str = "the scripted agent received the task";
    const SCRIPTED_TOOL: &str = "inspect the workspace";
    const SCRIPTED_SUMMARY: &str = "the scripted run completed";
    const UNKNOWN_TASK: &str = "00000000-0000-7000-8000-000000000000";

    fn scripted_state(
        dir: &TempDir,
        token: Option<&str>,
    ) -> AppState {
        scripted_state_with(dir, token, Vec::new())
    }

    fn scripted_state_with(
        dir: &TempDir,
        token: Option<&str>,
        transport_hosts: Vec<String>,
    ) -> AppState {
        let runner = ScriptedRunner::new(
            vec![
                JournalEvent::Message {
                    text: SCRIPTED_TEXT.to_owned(),
                },
                JournalEvent::ToolCall {
                    title: SCRIPTED_TOOL.to_owned(),
                    status: "completed".to_owned(),
                },
            ],
            SCRIPTED_SUMMARY,
        );
        state_with(dir, token, transport_hosts, Arc::new(runner))
    }

    fn scripted_state_with_journal(
        dir: &TempDir,
        journal: Arc<dyn Journal>,
    ) -> AppState {
        let runner = ScriptedRunner::new(
            vec![
                JournalEvent::Message {
                    text: SCRIPTED_TEXT.to_owned(),
                },
                JournalEvent::ToolCall {
                    title: SCRIPTED_TOOL.to_owned(),
                    status: "completed".to_owned(),
                },
            ],
            SCRIPTED_SUMMARY,
        );
        state_with_journal(dir, None, Vec::new(), Arc::new(runner), journal)
    }

    fn many_update_state(
        dir: &TempDir,
        count: usize,
    ) -> AppState {
        state_with(dir, None, Vec::new(), Arc::new(many_update_runner(count)))
    }

    fn many_update_runner(count: usize) -> ScriptedRunner {
        let events = (0..count)
            .map(|index| JournalEvent::Message {
                text: format!("u{index}"),
            })
            .collect();
        ScriptedRunner::new(events, SCRIPTED_SUMMARY)
    }

    fn file_journal_state(
        dir: &TempDir,
        count: usize,
    ) -> AppState {
        let journal: Arc<dyn Journal> = Arc::new(
            FileJournal::open(
                dir.path().join("journal"),
                JournalConfig {
                    max_bytes: 1,
                    ..JournalConfig::default()
                },
            )
            .expect("the file journal opens"),
        );
        state_with_journal(
            dir,
            None,
            Vec::new(),
            Arc::new(many_update_runner(count)),
            journal,
        )
    }

    fn state_with(
        dir: &TempDir,
        token: Option<&str>,
        transport_hosts: Vec<String>,
        runner: Arc<dyn Runner>,
    ) -> AppState {
        state_with_journal(
            dir,
            token,
            transport_hosts,
            runner,
            Arc::new(MemoryJournal::new()),
        )
    }

    fn state_with_journal(
        dir: &TempDir,
        token: Option<&str>,
        transport_hosts: Vec<String>,
        runner: Arc<dyn Runner>,
        journal: Arc<dyn Journal>,
    ) -> AppState {
        let store: Arc<dyn EnumerableStore<TaskId, ControlEvent>> = Arc::new(
            JsonlEventStore::<TaskId, ControlEvent>::open(dir.path().join("tasks"))
                .expect("the store opens"),
        );
        build_state(
            store,
            journal,
            runner,
            ControlConfig {
                agents: vec!["scripted".to_owned()],
                default_agent: "scripted".to_owned(),
                transport_hosts,
                allowed_hosts: allowed_hosts(
                    "127.0.0.1:8787".parse().expect("a loopback address"),
                    &[],
                ),
                workers: 1,
                token: token.map(str::to_owned),
            },
        )
    }

    struct FailingJournal;

    impl Journal for FailingJournal {
        fn append(
            &self,
            _task: &str,
            _events: &[JournalEvent],
        ) -> Result<Vec<JournalEntry>, JournalError> {
            Err(JournalError::Io(
                "/srv/mokmok/journal: the disk is full".to_owned(),
            ))
        }

        fn tail(
            &self,
            _task: &str,
            _limit: usize,
        ) -> Result<JournalTail, JournalError> {
            Err(JournalError::Io(
                "/srv/mokmok/journal: the disk is full".to_owned(),
            ))
        }

        fn count(
            &self,
            _task: &str,
        ) -> Result<u64, JournalError> {
            Err(JournalError::Io(
                "/srv/mokmok/journal: the disk is full".to_owned(),
            ))
        }
    }

    struct PanickingJournal;

    impl Journal for PanickingJournal {
        fn append(
            &self,
            _task: &str,
            _events: &[JournalEvent],
        ) -> Result<Vec<JournalEntry>, JournalError> {
            Ok(Vec::new())
        }

        fn tail(
            &self,
            _task: &str,
            _limit: usize,
        ) -> Result<JournalTail, JournalError> {
            panic!("the journal read panics");
        }

        fn count(
            &self,
            _task: &str,
        ) -> Result<u64, JournalError> {
            Ok(0)
        }
    }

    // A journal whose tail blocks until the test releases it, so a test can count how
    // many reads are in flight.
    struct BlockingJournal {
        entered: tokio::sync::mpsc::UnboundedSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl Journal for BlockingJournal {
        fn append(
            &self,
            _task: &str,
            _events: &[JournalEvent],
        ) -> Result<Vec<JournalEntry>, JournalError> {
            Ok(Vec::new())
        }

        fn tail(
            &self,
            _task: &str,
            _limit: usize,
        ) -> Result<JournalTail, JournalError> {
            self.entered
                .send(())
                .expect("the test listens for the read");
            // A failed assertion can leave a read blocked, so the wait is bounded rather
            // than waiting for a release the test will never send.
            self.release
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv_timeout(Duration::from_secs(5))
                .expect("the test releases the read");
            Ok(JournalTail {
                entries: Vec::new(),
                total: 0,
            })
        }

        fn count(
            &self,
            _task: &str,
        ) -> Result<u64, JournalError> {
            Ok(0)
        }
    }

    struct Reply {
        status: StatusCode,
        headers: HeaderMap,
        body: String,
    }

    impl Reply {
        fn json(&self) -> Value {
            serde_json::from_str(&self.body).expect("the body is JSON")
        }
    }

    async fn send(
        state: &AppState,
        request: Request<Body>,
    ) -> Reply {
        let response = build_router(state.clone())
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("the body collects")
            .to_bytes();
        Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    async fn get(
        state: &AppState,
        path: &str,
    ) -> Reply {
        let request = Request::builder()
            .uri(path)
            .header(header::HOST, "localhost")
            .body(Body::empty())
            .expect("the request builds");
        send(state, request).await
    }

    fn post_request(
        path: &str,
        body: &Value,
        authorization: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::HOST, "localhost")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        builder
            .body(Body::from(body.to_string()))
            .expect("the request builds")
    }

    async fn post_json(
        state: &AppState,
        path: &str,
        body: &Value,
    ) -> Reply {
        send(state, post_request(path, body, None)).await
    }

    fn new_task(agent: &str) -> Value {
        json!({
            "title": "Fix the build",
            "instruction": "Make the tests pass.",
            "agent": agent,
        })
    }

    fn legacy_log(
        id: &TaskId,
        title: &str,
        updates: Vec<AgentUpdate>,
    ) -> Vec<CloudEvent<ControlEvent>> {
        let subject = id.as_str();
        let stamp = |version: u64| format!("2026-11-01T00:00:{:02}.000Z", version - 1);
        let spec = TaskSpec {
            task_id: id.clone(),
            title: title.to_owned(),
            instruction: "Keep replaying.".to_owned(),
            agent: "scripted".to_owned(),
            host: None,
            workspace: None,
        };
        let mut events = vec![
            CloudEvent::new(subject, Version::new(1), ControlEvent::Submitted { spec })
                .with_time(stamp(1)),
            CloudEvent::new(subject, Version::new(2), ControlEvent::Started).with_time(stamp(2)),
        ];
        for update in updates {
            let version = u64::try_from(events.len()).expect("the log fits") + 1;
            events.push(
                CloudEvent::new(
                    subject,
                    Version::new(version),
                    ControlEvent::UpdateRecorded { update },
                )
                .with_time(stamp(version)),
            );
        }
        let version = u64::try_from(events.len()).expect("the log fits") + 1;
        events.push(
            CloudEvent::new(
                subject,
                Version::new(version),
                ControlEvent::Completed {
                    summary: "done".to_owned(),
                },
            )
            .with_time(stamp(version)),
        );
        events
    }

    fn created_id(reply: &Reply) -> String {
        reply.json()["id"]
            .as_str()
            .expect("the id is a string")
            .to_owned()
    }

    fn content_type(reply: &Reply) -> &str {
        reply
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .expect("the response names its content type")
    }

    #[tokio::test]
    async fn the_daemon_serves_no_ui() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        for path in ["/", "/app.js", "/app.css"] {
            let reply = get(&state, path).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        }
        assert_eq!(get(&state, "/healthz").await.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn healthz_answers_ok() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);
        let reply = get(&state, "/healthz").await;

        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(content_type(&reply), "text/plain; charset=utf-8");
        assert_eq!(reply.body, "ok");
    }

    #[tokio::test]
    async fn the_agent_list_names_the_default() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);
        let reply = get(&state, "/api/agents").await;

        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["agents"], json!(["scripted"]));
        assert_eq!(payload["default"], json!("scripted"));
    }

    #[tokio::test]
    async fn the_task_list_starts_empty() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);
        let reply = get(&state, "/api/tasks").await;

        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.json()["tasks"], json!([]));
    }

    #[tokio::test]
    async fn a_submitted_task_runs_and_reports_its_transcript() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let created = post_json(&state, "/api/tasks", &new_task("scripted")).await;
        assert_eq!(created.status, StatusCode::CREATED);
        let id = created_id(&created);

        assert_eq!(state.supervisor.run_pending().expect("the task runs"), 1);

        let listed = get(&state, "/api/tasks").await;
        assert_eq!(listed.status, StatusCode::OK);
        let tasks = listed.json()["tasks"].clone();
        assert_eq!(tasks.as_array().map(Vec::len), Some(1));
        assert_eq!(tasks[0]["id"], json!(id));
        assert_eq!(tasks[0]["status"], json!("completed"));
        assert_eq!(tasks[0]["title"], json!("Fix the build"));
        assert_eq!(tasks[0]["instruction"], json!("Make the tests pass."));
        assert_eq!(tasks[0]["instruction_truncated"], json!(false));
        assert_eq!(tasks[0]["agent"], json!("scripted"));
        assert_eq!(tasks[0]["outcome"], json!(SCRIPTED_SUMMARY));
        assert!(
            tasks[0].get("workspace").is_none(),
            "a task without a workspace omits the field"
        );

        let detail = get(&state, &format!("/api/tasks/{}", id.as_str())).await;
        assert_eq!(detail.status, StatusCode::OK);
        let payload = detail.json();
        let task = &payload["state"];
        assert_eq!(task["status"], json!("completed"));
        assert_eq!(task["spec"]["title"], json!("Fix the build"));
        assert_eq!(task["spec"]["agent"], json!("scripted"));
        assert_eq!(task["session"], json!("scripted-session"));
        assert_eq!(task["update_count"], json!(0));
        assert!(
            task.get("updates").is_none(),
            "an empty transcript is omitted"
        );
        assert_eq!(task["outcome"], json!(SCRIPTED_SUMMARY));

        let events = payload["events"].as_array().expect("events are a list");
        assert_eq!(payload["event_count"], json!(4));
        assert_eq!(events.len(), 4);
        assert_eq!(events[0]["type"], json!("dev.mokmok.control.submitted"));
        assert_eq!(events[1]["type"], json!("dev.mokmok.control.started"));
        assert_eq!(
            events[2]["type"],
            json!("dev.mokmok.control.session_opened")
        );
        assert_eq!(events[3]["type"], json!("dev.mokmok.control.completed"));

        let journal = get(&state, &format!("/api/tasks/{}/journal", id.as_str())).await;
        assert_eq!(journal.status, StatusCode::OK);
        let payload = journal.json();
        assert_eq!(payload["total"], json!(2));
        let entries = payload["entries"].as_array().expect("entries are a list");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["seq"], json!(1));
        assert_eq!(entries[0]["kind"], json!("message"));
        assert_eq!(entries[0]["text"], json!(SCRIPTED_TEXT));
        assert!(
            entries[0]["time"]
                .as_str()
                .is_some_and(|time| time.ends_with('Z')),
            "{}",
            entries[0]
        );
        assert_eq!(entries[1]["seq"], json!(2));
        assert_eq!(entries[1]["kind"], json!("tool_call"));
        assert_eq!(entries[1]["title"], json!(SCRIPTED_TOOL));
        assert_eq!(entries[1]["status"], json!("completed"));
    }

    async fn completed_task(state: &AppState) -> String {
        let created = post_json(state, "/api/tasks", &new_task("scripted")).await;
        assert_eq!(created.status, StatusCode::CREATED);
        let id = created_id(&created);
        assert_eq!(state.supervisor.run_pending().expect("the task runs"), 1);
        id
    }

    #[tokio::test]
    async fn the_journal_tail_is_bounded_by_default() {
        let dir = TempDir::new().expect("a temp dir");
        let state = many_update_state(&dir, 250);
        let id = completed_task(&state).await;

        let reply = get(&state, &format!("/api/tasks/{}/journal", id.as_str())).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["total"], json!(250));
        let entries = payload["entries"].as_array().expect("entries are a list");
        assert_eq!(entries.len(), 200);
        assert_eq!(entries[0]["seq"], json!(51));
        assert_eq!(
            entries.last().expect("the tail has an entry")["seq"],
            json!(250)
        );
        assert_eq!(
            entries.last().expect("the tail has an entry")["text"],
            json!("u249")
        );
    }

    #[tokio::test]
    async fn a_journal_limit_selects_the_newest_entries() {
        let dir = TempDir::new().expect("a temp dir");
        let state = many_update_state(&dir, 10);
        let id = completed_task(&state).await;

        let reply = get(&state, &format!("/api/tasks/{id}/journal?limit=3")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["total"], json!(10));
        let entries = payload["entries"].as_array().expect("entries are a list");
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry["seq"].clone())
                .collect::<Vec<_>>(),
            vec![json!(8), json!(9), json!(10)]
        );
    }

    #[tokio::test]
    async fn the_journal_limit_accepts_its_boundaries() {
        let dir = TempDir::new().expect("a temp dir");
        let state = many_update_state(&dir, 1000);
        let id = completed_task(&state).await;

        let reply = get(&state, &format!("/api/tasks/{id}/journal?limit=1")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["total"], json!(1000));
        assert_eq!(payload["entries"].as_array().map(Vec::len), Some(1));

        let reply = get(&state, &format!("/api/tasks/{id}/journal?limit=1000")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["total"], json!(1000));
        assert_eq!(payload["entries"].as_array().map(Vec::len), Some(1000));
    }

    #[tokio::test]
    async fn a_bad_journal_limit_is_rejected() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        for query in [
            "?limit=0",
            "?limit=1001",
            "?limit=bad",
            "?limit=1.5",
            "?limit=-1",
            "?limit=5&limit=6",
            "?limit=",
        ] {
            let reply = get(&state, &format!("/api/tasks/{UNKNOWN_TASK}/journal{query}")).await;
            assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{query}");
            assert!(
                reply.json()["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("limit")),
                "{}",
                reply.body
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_task_has_no_journal() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let reply = get(&state, &format!("/api/tasks/{UNKNOWN_TASK}/journal")).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
        assert_eq!(reply.json()["error"], json!("no such task"));
    }

    #[tokio::test]
    async fn a_submitted_task_has_an_empty_journal() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let created = post_json(&state, "/api/tasks", &new_task("scripted")).await;
        assert_eq!(created.status, StatusCode::CREATED);
        let id = created_id(&created);

        let reply = get(&state, &format!("/api/tasks/{id}/journal")).await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(
            reply.json(),
            json!({"entries": [], "total": 0}),
            "a task that has not run has no transcript"
        );

        let listed = get(&state, "/api/tasks").await;
        assert_eq!(listed.json()["tasks"][0]["updates"], json!(0));
    }

    #[tokio::test]
    async fn a_file_journal_serves_rotated_segments_in_order() {
        let dir = TempDir::new().expect("a temp dir");
        let state = file_journal_state(&dir, 3);
        let id = completed_task(&state).await;

        let segment = dir.path().join("journal").join(&id).join("2.jsonl");
        assert!(
            segment.exists(),
            "the third entry rotated to its own segment"
        );

        let reply = get(&state, &format!("/api/tasks/{id}/journal")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["total"], json!(3));
        let entries = payload["entries"].as_array().expect("entries are a list");
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry["seq"].clone())
                .collect::<Vec<_>>(),
            vec![json!(1), json!(2), json!(3)]
        );
    }

    #[tokio::test]
    async fn a_long_turn_keeps_the_domain_log_small_and_the_journal_complete() {
        let dir = TempDir::new().expect("a temp dir");
        let state = many_update_state(&dir, 200);
        let id = completed_task(&state).await;

        let reply = get(&state, &format!("/api/tasks/{id}?events=all")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        let events = payload["events"].as_array().expect("events are a list");
        assert_eq!(events.len(), 4, "a turn must not grow the domain log");
        assert_eq!(events[0]["type"], json!("dev.mokmok.control.submitted"));
        assert_eq!(events[1]["type"], json!("dev.mokmok.control.started"));
        assert_eq!(
            events[2]["type"],
            json!("dev.mokmok.control.session_opened")
        );
        assert_eq!(events[3]["type"], json!("dev.mokmok.control.completed"));
        assert_eq!(payload["state"]["status"], json!("completed"));
        assert_eq!(payload["state"]["update_count"], json!(0));
        assert!(
            payload["state"].get("updates").is_none(),
            "an empty transcript is omitted"
        );

        let reply = get(&state, &format!("/api/tasks/{id}/journal?limit=200")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["total"], json!(200));
        let entries = payload["entries"].as_array().expect("entries are a list");
        assert_eq!(entries.len(), 200);
        for (index, entry) in entries.iter().enumerate() {
            assert_eq!(entry["seq"], json!(index + 1));
            assert_eq!(entry["kind"], json!("message"));
            assert_eq!(entry["text"], json!(format!("u{index}")));
        }
    }

    #[tokio::test]
    async fn a_legacy_log_replays_and_projects_its_updates() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);
        let id = TaskId::from_u128(42);
        let legacy = legacy_log(
            &id,
            "An older task",
            vec![
                AgentUpdate::Message {
                    text: "from the old log".to_owned(),
                },
                AgentUpdate::Thought {
                    text: "second thought".to_owned(),
                },
                AgentUpdate::ToolCall {
                    title: "third".to_owned(),
                    status: "completed".to_owned(),
                },
            ],
        );
        state
            .store
            .append(&id, Version::ZERO, &legacy)
            .expect("the legacy log appends");

        let detail = get(&state, &format!("/api/tasks/{}", id.as_str())).await;
        assert_eq!(detail.status, StatusCode::OK);
        let payload = detail.json();
        assert_eq!(payload["state"]["status"], json!("completed"));
        assert_eq!(payload["state"]["update_count"], json!(3));

        let journal = get(
            &state,
            &format!("/api/tasks/{}/journal?limit=2", id.as_str()),
        )
        .await;
        assert_eq!(journal.status, StatusCode::OK);
        let payload = journal.json();
        assert_eq!(payload["total"], json!(3));
        assert_eq!(
            payload["entries"],
            json!([
                {
                    "seq": 4,
                    "time": "2026-11-01T00:00:03.000Z",
                    "kind": "thought",
                    "text": "second thought",
                },
                {
                    "seq": 5,
                    "time": "2026-11-01T00:00:04.000Z",
                    "kind": "tool_call",
                    "title": "third",
                    "status": "completed",
                },
            ])
        );
        assert_eq!(
            state.journal.count(id.as_str()).expect("the count reads"),
            0,
            "the projection is not written back"
        );

        let listed = get(&state, "/api/tasks").await;
        assert_eq!(listed.status, StatusCode::OK);
        assert_eq!(
            listed.json()["tasks"][0]["updates"],
            json!(3),
            "a legacy task falls back to the domain count"
        );
    }

    #[tokio::test]
    async fn a_legacy_projection_is_clipped_to_the_record_bound() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);
        let id = TaskId::from_u128(46);
        let legacy = legacy_log(
            &id,
            "A huge task",
            vec![AgentUpdate::Message {
                text: "x".repeat(MAX_RECORD_BYTES + 1),
            }],
        );
        state
            .store
            .append(&id, Version::ZERO, &legacy)
            .expect("the legacy log appends");

        let reply = get(&state, &format!("/api/tasks/{}/journal", id.as_str())).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        let text = payload["entries"][0]["text"]
            .as_str()
            .expect("the projected entry carries text");
        assert_eq!(text.len(), MAX_RECORD_BYTES);
    }

    #[tokio::test]
    async fn a_journal_wins_over_legacy_updates() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);
        let id = TaskId::from_u128(43);
        let legacy = legacy_log(
            &id,
            "A mixed task",
            vec![AgentUpdate::Message {
                text: "legacy only".to_owned(),
            }],
        );
        state
            .store
            .append(&id, Version::ZERO, &legacy)
            .expect("the legacy log appends");
        state
            .journal
            .append(
                id.as_str(),
                &[
                    JournalEvent::Message {
                        text: "journal wins".to_owned(),
                    },
                    JournalEvent::ToolCall {
                        title: "new tool".to_owned(),
                        status: "completed".to_owned(),
                    },
                ],
            )
            .expect("the journal appends");

        let reply = get(&state, &format!("/api/tasks/{}/journal", id.as_str())).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["total"], json!(2));
        let entries = payload["entries"].as_array().expect("entries are a list");
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry["text"].clone())
                .collect::<Vec<_>>(),
            vec![json!("journal wins"), Value::Null],
            "once the journal has entries the legacy prefix is not shown"
        );
        assert_eq!(entries[1]["title"], json!("new tool"));
    }

    #[tokio::test]
    async fn the_task_list_counts_the_journal() {
        let dir = TempDir::new().expect("a temp dir");
        let state = many_update_state(&dir, 3);
        let id = completed_task(&state).await;

        let listed = get(&state, "/api/tasks").await;
        assert_eq!(listed.status, StatusCode::OK);
        let tasks = listed.json()["tasks"].clone();
        assert_eq!(tasks[0]["id"], json!(id));
        assert_eq!(tasks[0]["updates"], json!(3));
        assert_eq!(tasks[0]["instruction"], json!("Make the tests pass."));
    }

    #[tokio::test]
    async fn the_task_list_names_the_workspace_and_outcome() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);
        let created = post_json(
            &state,
            "/api/tasks",
            &json!({
                "title": "Fix the build",
                "instruction": "Make the tests pass.",
                "agent": "scripted",
                "workspace": "/work/demo",
            }),
        )
        .await;
        let id = created_id(&created);
        assert_eq!(state.supervisor.run_pending().expect("the task runs"), 1);

        let listed = get(&state, "/api/tasks").await;
        assert_eq!(listed.status, StatusCode::OK);
        let task = &listed.json()["tasks"][0];
        assert_eq!(task["id"], json!(id));
        assert_eq!(task["workspace"], json!("/work/demo"));
        assert_eq!(task["instruction"], json!("Make the tests pass."));
        assert_eq!(task["outcome"], json!(SCRIPTED_SUMMARY));
    }

    #[tokio::test]
    async fn the_task_list_caps_the_instruction_preview() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);
        let instruction = "漢".repeat(2000);
        let created = post_json(
            &state,
            "/api/tasks",
            &json!({
                "title": "A huge task",
                "instruction": instruction.clone(),
                "agent": "scripted",
            }),
        )
        .await;
        let id = created_id(&created);
        assert_eq!(state.supervisor.run_pending().expect("the task runs"), 1);

        let listed = get(&state, "/api/tasks").await;
        assert_eq!(listed.status, StatusCode::OK);
        let task = &listed.json()["tasks"][0];
        let preview = task["instruction"].as_str().expect("a preview");
        assert!(
            preview.ends_with('…'),
            "a clipped preview carries the marker: {preview:?}"
        );
        assert!(
            preview.len() <= 4096 + '…'.len_utf8(),
            "the preview stays near the cap: {} bytes",
            preview.len()
        );
        assert!(
            preview.len() > 4000,
            "the preview keeps most of the instruction: {} bytes",
            preview.len()
        );
        assert_eq!(task["instruction_truncated"], json!(true));

        let detail = get(&state, &format!("/api/tasks/{}?events=none", id.as_str())).await;
        assert_eq!(detail.status, StatusCode::OK);
        assert_eq!(
            detail.json()["state"]["spec"]["instruction"],
            json!(instruction)
        );
    }

    #[tokio::test]
    async fn a_journal_failure_is_a_generic_500_and_the_list_still_answers() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state_with_journal(&dir, Arc::new(FailingJournal));
        let id = completed_task(&state).await;
        let legacy_id = TaskId::from_u128(44);
        let legacy = legacy_log(
            &legacy_id,
            "A legacy task",
            vec![AgentUpdate::Message {
                text: "old".to_owned(),
            }],
        );
        state
            .store
            .append(&legacy_id, Version::ZERO, &legacy)
            .expect("the legacy log appends");

        let reply = get(&state, &format!("/api/tasks/{id}/journal")).await;
        assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            reply.json()["error"],
            json!("the journal could not be read")
        );
        assert!(
            !reply.body.contains(&dir.path().display().to_string()),
            "the body must not carry a path: {}",
            reply.body
        );

        let listed = get(&state, "/api/tasks").await;
        assert_eq!(listed.status, StatusCode::OK);
        let tasks = listed.json()["tasks"].clone();
        let updates = |wanted: &str| {
            tasks
                .as_array()
                .and_then(|tasks| tasks.iter().find(|task| task["id"] == json!(wanted)))
                .map(|task| task["updates"].clone())
        };
        assert_eq!(
            updates(legacy_id.as_str()),
            Some(json!(1)),
            "a failed count leaves the domain count"
        );
        assert_eq!(updates(id.as_str()), Some(json!(0)));
    }

    #[tokio::test]
    async fn a_panicking_journal_read_is_a_generic_500() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state_with_journal(&dir, Arc::new(PanickingJournal));
        let id = completed_task(&state).await;

        // There are two permits, so a panic that kept its permit would fail the third
        // request rather than the first. Each request has a deadline, so a leaked permit
        // fails here instead of hanging the suite.
        for attempt in 1..=4 {
            let reply = tokio::time::timeout(
                Duration::from_secs(5),
                get(&state, &format!("/api/tasks/{id}/journal")),
            )
            .await
            .expect("the request waits forever for a leaked permit");
            assert_eq!(
                reply.status,
                StatusCode::INTERNAL_SERVER_ERROR,
                "request {attempt}"
            );
            assert_eq!(
                reply.json()["error"],
                json!("the journal could not be read")
            );
            assert!(
                !reply.body.contains("panics"),
                "the body must not carry the panic: {}",
                reply.body
            );
        }
    }

    fn spawn_journal_read(
        state: &AppState,
        path: String,
    ) -> tokio::task::JoinHandle<StatusCode> {
        let router = build_router(state.clone());
        tokio::spawn(async move {
            let request = Request::builder()
                .uri(path)
                .header(header::HOST, "localhost")
                .body(Body::empty())
                .expect("the request builds");
            router
                .oneshot(request)
                .await
                .expect("the router answers")
                .status()
        })
    }

    async fn await_journal_read(reads: &mut tokio::sync::mpsc::UnboundedReceiver<()>) {
        tokio::time::timeout(Duration::from_secs(10), reads.recv())
            .await
            .expect("a read starts")
            .expect("the read signals its start");
    }

    #[tokio::test]
    async fn the_journal_read_semaphore_bounds_concurrent_reads() {
        let dir = TempDir::new().expect("a temp dir");
        let (entered, mut reads) = tokio::sync::mpsc::unbounded_channel();
        let (release, held) = mpsc::channel();
        let state = scripted_state_with_journal(
            &dir,
            Arc::new(BlockingJournal {
                entered,
                release: Mutex::new(held),
            }),
        );
        let id = completed_task(&state).await;
        let path = format!("/api/tasks/{id}/journal");

        let first = spawn_journal_read(&state, path.clone());
        let second = spawn_journal_read(&state, path.clone());
        await_journal_read(&mut reads).await;
        await_journal_read(&mut reads).await;
        // Only two reads are in, so an exhausted pool is exactly a pool of two: a wider
        // bound leaves a permit free here.
        assert_eq!(
            state.journal_reads.available_permits(),
            0,
            "two reads must hold the whole journal-read pool"
        );

        let third = spawn_journal_read(&state, path);
        // Poll the third task once, so it has reached the semaphore: before that it has
        // not run at all and the negative assertion below is trivially true.
        tokio::task::yield_now().await;
        assert!(
            reads.try_recv().is_err(),
            "a third read started while both permits were held"
        );
        // Only a release can admit the third read, since no permit is free.
        release.send(()).expect("a read waits for a release");
        await_journal_read(&mut reads).await;

        release.send(()).expect("a read waits for a release");
        release.send(()).expect("a read waits for a release");
        for request in [first, second, third] {
            let status = tokio::time::timeout(Duration::from_secs(10), request)
                .await
                .expect("the request answers")
                .expect("the request task completes");
            assert_eq!(status, StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn a_dropped_journal_request_keeps_its_reads_permit() {
        let dir = TempDir::new().expect("a temp dir");
        let (entered, mut reads) = tokio::sync::mpsc::unbounded_channel();
        let (release, held) = mpsc::channel();
        let state = scripted_state_with_journal(
            &dir,
            Arc::new(BlockingJournal {
                entered,
                release: Mutex::new(held),
            }),
        );
        let id = completed_task(&state).await;
        let path = format!("/api/tasks/{id}/journal");

        let first = spawn_journal_read(&state, path.clone());
        let second = spawn_journal_read(&state, path);
        await_journal_read(&mut reads).await;
        await_journal_read(&mut reads).await;
        assert_eq!(state.journal_reads.available_permits(), 0);

        // Both clients go away while their reads are still inside the journal.
        first.abort();
        second.abort();
        for request in [first, second] {
            assert!(
                request
                    .await
                    .expect_err("the request was cancelled")
                    .is_cancelled()
            );
        }
        assert_eq!(
            state.journal_reads.available_permits(),
            0,
            "a dropped request must not release the permit its read still needs"
        );

        release.send(()).expect("a read waits for a release");
        release.send(()).expect("a read waits for a release");
    }

    #[tokio::test]
    async fn events_none_still_replays_the_state() {
        let dir = TempDir::new().expect("a temp dir");
        let state = many_update_state(&dir, 250);
        let id = completed_task(&state).await;

        let reply = get(&state, &format!("/api/tasks/{id}?events=none")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["event_count"], json!(4));
        assert_eq!(payload["events"], json!([]));
        assert_eq!(payload["state"]["status"], json!("completed"));
        assert_eq!(payload["state"]["update_count"], json!(0));
    }

    #[tokio::test]
    async fn events_all_returns_every_event() {
        let dir = TempDir::new().expect("a temp dir");
        let state = many_update_state(&dir, 250);
        let id = completed_task(&state).await;

        let reply = get(&state, &format!("/api/tasks/{id}?events=all")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["event_count"], json!(4));
        let events = payload["events"].as_array().expect("events are a list");
        assert_eq!(events.len(), 4);
        assert_eq!(events[0]["type"], json!("dev.mokmok.control.submitted"));
        assert_eq!(
            events.last().expect("the log has an event")["type"],
            json!("dev.mokmok.control.completed")
        );
    }

    #[tokio::test]
    async fn a_bad_events_value_is_rejected() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let reply = get(&state, &format!("/api/tasks/{UNKNOWN_TASK}?events=bad")).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        assert!(
            reply.json()["error"]
                .as_str()
                .is_some_and(|error| error.contains("events")),
            "{}",
            reply.body
        );
    }

    #[tokio::test]
    async fn an_empty_title_is_rejected() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let reply = post_json(
            &state,
            "/api/tasks",
            &json!({"title": "", "instruction": "Do it.", "agent": "scripted"}),
        )
        .await;

        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        assert!(
            reply.json()["error"]
                .as_str()
                .is_some_and(|error| error.contains("title"))
        );
        assert_eq!(state.system.list().expect("the list reads").len(), 0);
    }

    #[tokio::test]
    async fn an_unknown_agent_is_rejected() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let reply = post_json(&state, "/api/tasks", &new_task("ghost")).await;

        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        assert_eq!(reply.json()["error"], json!("no agent is named \"ghost\""));
        assert_eq!(state.system.list().expect("the list reads").len(), 0);
    }

    #[tokio::test]
    async fn a_malformed_id_is_rejected() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let detail = get(&state, "/api/tasks/not-a-uuid").await;
        assert_eq!(detail.status, StatusCode::BAD_REQUEST);
        assert!(
            detail.json()["error"]
                .as_str()
                .is_some_and(|error| error.contains("task id"))
        );

        let cancel = post_json(&state, "/api/tasks/not-a-uuid/cancel", &json!({})).await;
        assert_eq!(cancel.status, StatusCode::BAD_REQUEST);

        let journal = get(&state, "/api/tasks/not-a-uuid/journal").await;
        assert_eq!(journal.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_unknown_task_is_not_found() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let detail = get(&state, &format!("/api/tasks/{UNKNOWN_TASK}")).await;
        assert_eq!(detail.status, StatusCode::NOT_FOUND);
        assert_eq!(detail.json()["error"], json!("no such task"));

        let cancel = post_json(
            &state,
            &format!("/api/tasks/{UNKNOWN_TASK}/cancel"),
            &json!({}),
        )
        .await;
        assert_eq!(cancel.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cancelling_a_submitted_task_is_accepted() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let created = post_json(&state, "/api/tasks", &new_task("scripted")).await;
        let id = created_id(&created);

        let reply = post_json(&state, &format!("/api/tasks/{id}/cancel"), &json!({})).await;
        assert_eq!(reply.status, StatusCode::ACCEPTED);
        assert_eq!(reply.json()["id"], json!(id));

        let detail = get(&state, &format!("/api/tasks/{}", id.as_str())).await;
        assert_eq!(detail.json()["state"]["status"], json!("cancelled"));
        assert_eq!(
            detail.json()["state"]["outcome"],
            json!("cancelled from the control plane")
        );
    }

    #[tokio::test]
    async fn cancelling_a_finished_task_conflicts() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let created = post_json(&state, "/api/tasks", &new_task("scripted")).await;
        let id = created_id(&created);
        assert_eq!(state.supervisor.run_pending().expect("the task runs"), 1);

        let reply = post_json(
            &state,
            &format!("/api/tasks/{id}/cancel"),
            &json!({"reason": "too late"}),
        )
        .await;

        assert_eq!(reply.status, StatusCode::CONFLICT);
        assert_eq!(
            reply.json()["error"],
            json!("the task has already finished")
        );

        let detail = get(&state, &format!("/api/tasks/{}", id.as_str())).await;
        assert_eq!(detail.json()["state"]["status"], json!("completed"));
        assert_eq!(detail.json()["state"]["outcome"], json!(SCRIPTED_SUMMARY));
    }

    #[tokio::test]
    async fn a_missing_token_is_unauthorized() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, Some("secret"));

        let task = post_json(&state, "/api/tasks", &new_task("scripted")).await;
        assert_eq!(task.status, StatusCode::UNAUTHORIZED);
        assert_eq!(task.json()["error"], json!("a bearer token is required"));

        let list = get(&state, "/api/tasks").await;
        assert_eq!(list.status, StatusCode::UNAUTHORIZED);

        // A browser's EventSource cannot set a header, so a token-protected feed is
        // unreachable from the page and it falls back to polling.
        let events = get(&state, "/api/events").await;
        assert_eq!(events.status, StatusCode::UNAUTHORIZED);
        let journal = get(&state, "/api/journal/events").await;
        assert_eq!(journal.status, StatusCode::UNAUTHORIZED);
        let task_journal = get(&state, &format!("/api/tasks/{UNKNOWN_TASK}/journal")).await;
        assert_eq!(task_journal.status, StatusCode::UNAUTHORIZED);

        assert_eq!(get(&state, "/").await.status, StatusCode::NOT_FOUND);
        assert_eq!(get(&state, "/app.js").await.status, StatusCode::NOT_FOUND);
        assert_eq!(get(&state, "/healthz").await.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_wrong_token_is_unauthorized() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, Some("secret"));

        let reply = send(
            &state,
            post_request("/api/tasks", &new_task("scripted"), Some("Bearer wrong")),
        )
        .await;

        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
        assert_eq!(state.system.list().expect("the list reads").len(), 0);
    }

    #[tokio::test]
    async fn a_same_length_wrong_token_is_unauthorized() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, Some("secret"));

        // "wrong!" and "secret" are both six bytes, so only the content differs.
        let reply = send(
            &state,
            post_request("/api/tasks", &new_task("scripted"), Some("Bearer wrong!")),
        )
        .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_right_token_is_accepted() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, Some("secret"));

        let reply = send(
            &state,
            post_request("/api/tasks", &new_task("scripted"), Some("Bearer secret")),
        )
        .await;

        assert_eq!(reply.status, StatusCode::CREATED);
        assert_eq!(state.system.list().expect("the list reads").len(), 1);
    }

    #[tokio::test]
    async fn the_event_stream_publishes_a_cloud_event() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let request = Request::builder()
            .uri("/api/events")
            .header(header::HOST, "localhost")
            .body(Body::empty())
            .expect("the request builds");
        let response = build_router(state.clone())
            .oneshot(request)
            .await
            .expect("the stream opens");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );

        let created = post_json(&state, "/api/tasks", &new_task("scripted")).await;
        assert_eq!(created.status, StatusCode::CREATED);

        let mut body = response.into_body();
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await
            .expect("a frame arrives")
            .expect("the stream is open")
            .expect("the frame is data");
        let data = frame
            .into_data()
            .map_err(|_| "the frame carries no data")
            .expect("a data frame");
        let text = String::from_utf8_lossy(&data).into_owned();

        assert!(text.starts_with("data: "), "{text}");
        let payload: Value = serde_json::from_str(text.trim_start_matches("data: ").trim())
            .expect("the data line is a CloudEvent");
        assert_eq!(payload["type"], json!("dev.mokmok.control.submitted"));
        assert_eq!(payload["data"]["kind"], json!("submitted"));
        assert_eq!(payload["data"]["spec"]["title"], json!("Fix the build"));
    }

    #[tokio::test]
    async fn the_journal_stream_publishes_an_entry() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let request = Request::builder()
            .uri("/api/journal/events")
            .header(header::HOST, "localhost")
            .body(Body::empty())
            .expect("the request builds");
        let response = build_router(state.clone())
            .oneshot(request)
            .await
            .expect("the stream opens");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );

        let created = post_json(&state, "/api/tasks", &new_task("scripted")).await;
        let id = created_id(&created);
        assert_eq!(state.supervisor.run_pending().expect("the task runs"), 1);

        let mut body = response.into_body();
        let mut received = Vec::new();
        for _ in 0..2 {
            let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
                .await
                .expect("a frame arrives")
                .expect("the stream is open")
                .expect("the frame is data");
            let data = frame
                .into_data()
                .map_err(|_| "the frame carries no data")
                .expect("a data frame");
            let text = String::from_utf8_lossy(&data).into_owned();
            assert!(text.starts_with("data: "), "{text}");
            received.push(
                serde_json::from_str::<Value>(text.trim_start_matches("data: ").trim())
                    .expect("the data line names a task and an entry"),
            );
        }

        assert_eq!(received[0]["task"], json!(id));
        assert_eq!(received[0]["entry"]["seq"], json!(1));
        assert_eq!(received[0]["entry"]["kind"], json!("message"));
        assert_eq!(received[0]["entry"]["text"], json!(SCRIPTED_TEXT));
        assert_eq!(received[1]["task"], json!(id));
        assert_eq!(received[1]["entry"]["seq"], json!(2));
        assert_eq!(received[1]["entry"]["kind"], json!("tool_call"));
        assert_eq!(received[1]["entry"]["title"], json!(SCRIPTED_TOOL));
    }

    #[test]
    fn a_two_event_batch_publishes_two_journal_lines() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);
        let mut published = state.journal_events.subscribe();

        let entries = state
            .journal
            .append(
                UNKNOWN_TASK,
                &[
                    JournalEvent::Message {
                        text: "first".to_owned(),
                    },
                    JournalEvent::Message {
                        text: "second".to_owned(),
                    },
                ],
            )
            .expect("the batch appends");
        assert_eq!(entries.len(), 2);

        for (line, seq) in [(published.try_recv(), 1), (published.try_recv(), 2)] {
            let line = line.expect("every entry of the batch publishes a line");
            let payload: Value = serde_json::from_str(&line).expect("the line is JSON");
            assert_eq!(payload["task"], json!(UNKNOWN_TASK));
            assert_eq!(payload["entry"]["seq"], json!(seq));
        }
    }

    #[tokio::test]
    async fn the_journal_stream_ends_when_the_daemon_shuts_down() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let request = Request::builder()
            .uri("/api/journal/events")
            .header(header::HOST, "localhost")
            .body(Body::empty())
            .expect("the request builds");
        let response = build_router(state.clone())
            .oneshot(request)
            .await
            .expect("the stream opens");
        assert_eq!(response.status(), StatusCode::OK);

        state
            .shutdown
            .send(true)
            .expect("the open stream holds a receiver");
        let collected =
            tokio::time::timeout(Duration::from_secs(1), response.into_body().collect())
                .await
                .expect("the stream ends at once")
                .expect("the body collects");
        assert!(
            collected.to_bytes().is_empty(),
            "nothing was published before the shutdown"
        );
    }

    #[tokio::test]
    async fn an_unexpected_host_is_forbidden() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let request = Request::builder()
            .uri("/api/tasks")
            .header(header::HOST, "attacker.example")
            .body(Body::empty())
            .expect("the request builds");
        let reply = send(&state, request).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert_eq!(
            reply.json()["error"],
            json!("the Host header is not allowed")
        );

        let request = Request::builder()
            .uri("/api/journal/events")
            .header(header::HOST, "attacker.example")
            .body(Body::empty())
            .expect("the request builds");
        assert_eq!(send(&state, request).await.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_listed_host_is_answered() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let request = Request::builder()
            .uri("/healthz")
            .header(header::HOST, "127.0.0.1")
            .body(Body::empty())
            .expect("the request builds");
        assert_eq!(send(&state, request).await.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn the_loopback_names_work_with_and_without_the_port() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        for host in ["127.0.0.1:8787", "localhost:8787", "localhost", "127.0.0.1"] {
            let request = Request::builder()
                .uri("/healthz")
                .header(header::HOST, host)
                .body(Body::empty())
                .expect("the request builds");
            assert_eq!(
                send(&state, request).await.status,
                StatusCode::OK,
                "{host} must reach the daemon"
            );
        }

        let request = Request::builder()
            .uri("/healthz")
            .header(header::HOST, "evil.example:8787")
            .body(Body::empty())
            .expect("the request builds");
        assert_eq!(send(&state, request).await.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_agent_list_names_transport_overrides() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state_with(&dir, None, vec!["eu".to_owned(), "us".to_owned()]);
        let reply = get(&state, "/api/agents").await;

        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["agents"], json!(["scripted"]));
        assert_eq!(payload["default"], json!("scripted"));
        assert_eq!(payload["hosts"], json!(["eu", "us"]));
    }

    #[tokio::test]
    async fn the_body_limit_accepts_128_kib_and_refuses_one_byte_more() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);
        let base = new_task("scripted").to_string();
        let limit = 128 * 1024;

        let mut exact = base.clone();
        exact.push_str(&" ".repeat(limit - exact.len()));
        assert_eq!(exact.len(), limit);
        let accepted = send(
            &state,
            Request::builder()
                .method("POST")
                .uri("/api/tasks")
                .header(header::HOST, "localhost")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(exact))
                .expect("the request builds"),
        )
        .await;
        assert_eq!(accepted.status, StatusCode::CREATED);

        let mut over = base;
        over.push_str(&" ".repeat(limit + 1 - over.len()));
        let refused = send(
            &state,
            Request::builder()
                .method("POST")
                .uri("/api/tasks")
                .header(header::HOST, "localhost")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(over))
                .expect("the request builds"),
        )
        .await;
        assert_eq!(refused.status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn the_bearer_scheme_is_case_insensitive() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, Some("secret"));

        for authorization in ["bearer secret", "BEARER secret", "Bearer secret"] {
            let reply = send(
                &state,
                post_request("/api/tasks", &new_task("scripted"), Some(authorization)),
            )
            .await;
            assert_eq!(
                reply.status,
                StatusCode::CREATED,
                "{authorization} should be accepted"
            );
        }
    }

    #[tokio::test]
    async fn a_401_names_the_bearer_scheme() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, Some("secret"));

        let reply = get(&state, "/api/tasks").await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            reply
                .headers
                .get(header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer realm=\"mokmok\"")
        );
    }
}
