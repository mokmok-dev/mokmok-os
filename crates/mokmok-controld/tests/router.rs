//! The router: assets, the task API, auth, and the live feed.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode, header};
    use http_body_util::BodyExt;
    use mokmok_control::{
        AgentUpdate, ControlEvent, EnumerableStore, Runner, ScriptedRunner, TaskId,
    };
    use mokmok_controld::{AppState, ControlConfig, allowed_hosts, build_router, build_state};
    use mokmok_jobs_jsonl::JsonlEventStore;
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
                AgentUpdate::Message {
                    text: SCRIPTED_TEXT.to_owned(),
                },
                AgentUpdate::ToolCall {
                    title: SCRIPTED_TOOL.to_owned(),
                    status: "completed".to_owned(),
                },
            ],
            SCRIPTED_SUMMARY,
        );
        state_with(dir, token, transport_hosts, Arc::new(runner))
    }

    fn many_update_state(
        dir: &TempDir,
        count: usize,
    ) -> AppState {
        let updates = (0..count)
            .map(|index| AgentUpdate::Message {
                text: format!("u{index}"),
            })
            .collect();
        let runner = ScriptedRunner::new(updates, SCRIPTED_SUMMARY);
        state_with(dir, None, Vec::new(), Arc::new(runner))
    }

    fn state_with(
        dir: &TempDir,
        token: Option<&str>,
        transport_hosts: Vec<String>,
        runner: Arc<dyn Runner>,
    ) -> AppState {
        let store: Arc<dyn EnumerableStore<TaskId, ControlEvent>> = Arc::new(
            JsonlEventStore::<TaskId, ControlEvent>::open(dir.path().join("tasks"))
                .expect("the store opens"),
        );
        build_state(
            store,
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
    async fn the_page_is_served_uncached() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);
        let reply = get(&state, "/").await;

        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(content_type(&reply), "text/html; charset=utf-8");
        assert_eq!(
            reply
                .headers
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert!(reply.body.contains("mokmok control plane"));
    }

    #[tokio::test]
    async fn the_javascript_and_stylesheet_are_served() {
        let dir = TempDir::new().expect("a temp dir");
        let state = scripted_state(&dir, None);

        let javascript = get(&state, "/app.js").await;
        assert_eq!(javascript.status, StatusCode::OK);
        assert_eq!(content_type(&javascript), "text/javascript; charset=utf-8");
        assert!(javascript.body.contains("function connectEvents"));

        let stylesheet = get(&state, "/app.css").await;
        assert_eq!(stylesheet.status, StatusCode::OK);
        assert_eq!(content_type(&stylesheet), "text/css; charset=utf-8");
        assert!(stylesheet.body.contains(".task-row"));
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

        let detail = get(&state, &format!("/api/tasks/{id}")).await;
        assert_eq!(detail.status, StatusCode::OK);
        let payload = detail.json();
        let task = &payload["state"];
        assert_eq!(task["status"], json!("completed"));
        assert_eq!(task["spec"]["title"], json!("Fix the build"));
        assert_eq!(task["spec"]["agent"], json!("scripted"));
        assert_eq!(task["session"], json!("scripted-session"));
        assert_eq!(task["update_count"], json!(2));
        assert_eq!(
            task["updates"],
            json!([
                {"kind": "message", "text": SCRIPTED_TEXT},
                {"kind": "tool_call", "title": SCRIPTED_TOOL, "status": "completed"},
            ])
        );
        assert_eq!(task["outcome"], json!(SCRIPTED_SUMMARY));

        let events = payload["events"].as_array().expect("events are a list");
        assert_eq!(payload["event_count"], json!(6));
        assert_eq!(events.len(), 6);
        assert_eq!(events[0]["type"], json!("dev.mokmok.control.submitted"));
        assert_eq!(events[1]["type"], json!("dev.mokmok.control.started"));
        assert_eq!(
            events[2]["type"],
            json!("dev.mokmok.control.session_opened")
        );
        assert_eq!(events[5]["type"], json!("dev.mokmok.control.completed"));
    }

    async fn completed_task(state: &AppState) -> String {
        let created = post_json(state, "/api/tasks", &new_task("scripted")).await;
        assert_eq!(created.status, StatusCode::CREATED);
        let id = created_id(&created);
        assert_eq!(state.supervisor.run_pending().expect("the task runs"), 1);
        id
    }

    #[tokio::test]
    async fn the_detail_event_tail_is_bounded_by_default() {
        let dir = TempDir::new().expect("a temp dir");
        let state = many_update_state(&dir, 250);
        let id = completed_task(&state).await;

        let reply = get(&state, &format!("/api/tasks/{id}")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["event_count"], json!(254));
        let events = payload["events"].as_array().expect("events are a list");
        assert_eq!(events.len(), 200);
        assert_eq!(
            events.last().expect("the tail has an event")["type"],
            json!("dev.mokmok.control.completed")
        );
    }

    #[tokio::test]
    async fn events_none_still_replays_the_state() {
        let dir = TempDir::new().expect("a temp dir");
        let state = many_update_state(&dir, 250);
        let id = completed_task(&state).await;

        let reply = get(&state, &format!("/api/tasks/{id}?events=none")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["event_count"], json!(254));
        assert_eq!(payload["events"], json!([]));
        assert_eq!(payload["state"]["status"], json!("completed"));
        assert_eq!(payload["state"]["update_count"], json!(250));
    }

    #[tokio::test]
    async fn events_all_returns_every_event() {
        let dir = TempDir::new().expect("a temp dir");
        let state = many_update_state(&dir, 250);
        let id = completed_task(&state).await;

        let reply = get(&state, &format!("/api/tasks/{id}?events=all")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let payload = reply.json();
        assert_eq!(payload["event_count"], json!(254));
        let events = payload["events"].as_array().expect("events are a list");
        assert_eq!(events.len(), 254);
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

        let detail = get(&state, &format!("/api/tasks/{id}")).await;
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

        let detail = get(&state, &format!("/api/tasks/{id}")).await;
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

        assert_eq!(get(&state, "/").await.status, StatusCode::OK);
        assert_eq!(get(&state, "/app.js").await.status, StatusCode::OK);
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
