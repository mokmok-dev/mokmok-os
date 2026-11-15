//! The agent table's JSON shape and validation.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use mokmok_control::{AgentConfig, AgentTable, AgentTransport, ConfigError};

    const VALID: &str = r#"{
        "agents": {
            "remote": {
                "transport": "websocket",
                "url": "wss://agents.example/acp",
                "secret": "s3cret",
                "bearer": "token"
            },
            "local": { "transport": "stdio", "program": "/usr/bin/agent", "args": ["--once"] }
        },
        "default": "local"
    }"#;

    fn parse(text: &str) -> Result<AgentTable, ConfigError> {
        AgentTable::from_json(text)
    }

    fn error(text: &str) -> ConfigError {
        parse(text).expect_err("the table is invalid")
    }

    #[test]
    fn a_valid_table_lists_its_names_sorted() {
        let table = parse(VALID).expect("the table is valid");
        assert_eq!(table.names(), vec!["local", "remote"]);
        assert_eq!(table.default_name(), Some("local"));
    }

    #[test]
    fn resolve_returns_the_named_agent_or_the_default() {
        let table = parse(VALID).expect("the table is valid");
        let local = table.resolve("local").expect("local exists");
        assert!(matches!(
            &local.transport,
            AgentTransport::Stdio { program, args }
                if program == &PathBuf::from("/usr/bin/agent") && args == &["--once".to_owned()]
        ));
        let default = table.resolve("").expect("the default exists");
        assert!(matches!(
            &default.transport,
            AgentTransport::Stdio { program, .. } if program == &PathBuf::from("/usr/bin/agent")
        ));
    }

    #[test]
    fn resolve_refuses_an_unknown_name() {
        let table = parse(VALID).expect("the table is valid");
        assert!(matches!(
            table.resolve("missing"),
            Err(ConfigError::UnknownAgent(name)) if name == "missing"
        ));
    }

    #[test]
    fn a_single_agent_is_the_default() {
        let table = AgentTable {
            agents: [(
                "only".to_owned(),
                AgentConfig {
                    transport: AgentTransport::Stdio {
                        program: PathBuf::from("/usr/bin/agent"),
                        args: Vec::new(),
                    },
                },
            )]
            .into_iter()
            .collect(),
            hosts: BTreeMap::new(),
            default: None,
        };
        assert_eq!(table.default_name(), Some("only"));
        assert!(table.resolve("").is_ok());
    }

    #[test]
    fn a_table_with_several_agents_needs_a_default_for_an_empty_name() {
        let json = r#"{"agents": {
            "one": { "transport": "stdio", "program": "/bin/one" },
            "two": { "transport": "stdio", "program": "/bin/two" }
        }}"#;
        let table = parse(json).expect("the table is valid");
        assert_eq!(table.default_name(), None);
        assert_eq!(table.resolve("").err(), Some(ConfigError::NoDefaultAgent));
        assert!(matches!(
            &table.resolve("two").expect("two exists").transport,
            AgentTransport::Stdio { program, .. } if program == &PathBuf::from("/bin/two")
        ));
    }

    #[test]
    fn serde_defaults_the_args() {
        let json = r#"{"agents": {"one": {"transport": "stdio", "program": "/bin/one"}}}"#;
        let table = parse(json).expect("the table is valid");
        assert!(matches!(
            &table.resolve("one").expect("one exists").transport,
            AgentTransport::Stdio { args, .. } if args.is_empty()
        ));
    }

    #[test]
    fn malformed_json_is_reported() {
        assert!(matches!(error("{"), ConfigError::Json(_)));
        assert!(matches!(
            error(r#"{"agents": {"one": {"transport": "carrier-pigeon"}}}"#),
            ConfigError::Json(_)
        ));
    }

    #[test]
    fn an_empty_table_is_refused() {
        assert_eq!(error(r#"{"agents": {}}"#), ConfigError::NoAgents);
    }

    #[test]
    fn a_bad_agent_name_is_refused() {
        for name in [
            "Agent",
            "agent_one",
            "-agent",
            "agent!",
            "agents-with-more-than-32-characters",
        ] {
            let json = format!(
                r#"{{"agents": {{"{name}": {{"transport": "stdio", "program": "/bin/agent"}}}}}}"#
            );
            assert_eq!(
                error(&json),
                ConfigError::InvalidName(name.to_owned()),
                "{name} should be refused"
            );
        }
        let json = r#"{"agents": {"a": {"transport": "stdio", "program": "/bin/agent"}}}"#;
        assert!(parse(json).is_ok(), "a single letter is a valid name");
    }

    #[test]
    fn a_web_socket_entry_carries_its_credentials() {
        let table = parse(VALID).expect("the table is valid");
        assert!(matches!(
            &table.resolve("remote").expect("remote exists").transport,
            AgentTransport::WebSocket { url, secret, bearer }
                if url == "wss://agents.example/acp"
                    && secret.as_deref() == Some("s3cret")
                    && bearer.as_deref() == Some("token")
        ));
    }

    #[test]
    fn the_credentials_default_to_none() {
        let json = r#"{"agents": {
            "remote": { "transport": "websocket", "url": "ws://127.0.0.1:2419/ws" }
        }}"#;
        let table = parse(json).expect("the table is valid");
        assert!(matches!(
            &table.resolve("remote").expect("remote exists").transport,
            AgentTransport::WebSocket {
                secret: None,
                bearer: None,
                ..
            }
        ));
    }

    #[test]
    fn a_relative_program_is_refused() {
        let json = r#"{"agents": {"one": {"transport": "stdio", "program": "bin/one"}}}"#;
        assert_eq!(
            error(json),
            ConfigError::RelativeProgram {
                agent: "one".to_owned(),
                program: "bin/one".to_owned(),
            }
        );
    }

    #[test]
    fn only_web_socket_urls_are_accepted() {
        for url in ["http://agents.example/acp", "agents.example/acp", "wss:/x"] {
            let json =
                format!(r#"{{"agents": {{"one": {{"transport": "websocket", "url": "{url}"}}}}}}"#);
            assert_eq!(
                error(&json),
                ConfigError::InvalidUrl {
                    agent: "one".to_owned(),
                },
                "{url} should be refused"
            );
        }
        for url in [
            "wss://agents.example/acp",
            "wss://agents.example/acp?server-key=old",
            "ws://127.0.0.1:2419/ws",
            "ws://localhost:2419/ws",
            "ws://[::1]:2419/ws",
        ] {
            let json =
                format!(r#"{{"agents": {{"one": {{"transport": "websocket", "url": "{url}"}}}}}}"#);
            assert!(parse(&json).is_ok(), "{url} should be accepted");
        }
    }

    #[test]
    fn a_cleartext_remote_url_is_refused() {
        for url in [
            "ws://agents.example/acp",
            "ws://192.168.0.10:2419/ws",
            "ws://example.test:1/ws",
        ] {
            let json =
                format!(r#"{{"agents": {{"one": {{"transport": "websocket", "url": "{url}"}}}}}}"#);
            assert_eq!(
                error(&json),
                ConfigError::InsecureUrl {
                    agent: "one".to_owned(),
                },
                "{url} should be refused"
            );
        }
    }

    #[test]
    fn the_websocket_tag_is_not_snake_cased() {
        let json = r#"{"agents": {"one": {"transport": "web_socket", "url": "wss://agents.example/acp"}}}"#;
        assert!(
            matches!(error(json), ConfigError::Json(_)),
            "the tag is websocket, not web_socket"
        );
    }

    #[test]
    fn a_default_naming_no_agent_is_refused() {
        let json = r#"{"agents": {"one": {"transport": "stdio", "program": "/bin/one"}},
            "default": "two"}"#;
        assert_eq!(error(json), ConfigError::UnknownAgent("two".to_owned()));
    }

    #[test]
    fn a_host_override_is_parsed() {
        let json = r#"{
            "agents": {"one": {"transport": "stdio", "program": "/bin/one"}},
            "hosts": {
                "remote": {"transport": "websocket", "url": "wss://remote.example/acp"},
                "second": {"transport": "stdio", "program": "/bin/two", "args": ["--once"]},
                "prod-eu": {"transport": "stdio", "program": "/bin/three"}
            }
        }"#;
        let table = parse(json).expect("the table is valid");
        assert_eq!(table.hosts.len(), 3);
        assert!(table.hosts.contains_key("prod-eu"));
        assert!(matches!(
            &table.hosts["remote"],
            AgentTransport::WebSocket { url, .. } if url == "wss://remote.example/acp"
        ));
        assert!(matches!(
            &table.hosts["second"],
            AgentTransport::Stdio { program, args }
                if program == &PathBuf::from("/bin/two") && args == &["--once".to_owned()]
        ));
    }

    #[test]
    fn a_bad_host_name_is_refused() {
        for host in ["Remote", "host_1", "-host", "host name"] {
            let json = format!(
                r#"{{"agents": {{"one": {{"transport": "stdio", "program": "/bin/one"}}}},
                    "hosts": {{"{host}": {{"transport": "stdio", "program": "/bin/two"}}}}}}"#
            );
            assert_eq!(
                error(&json),
                ConfigError::InvalidHost(host.to_owned()),
                "{host} should be refused"
            );
        }
    }

    #[test]
    fn a_host_transport_follows_the_same_rules() {
        let relative = r#"{
            "agents": {"one": {"transport": "stdio", "program": "/bin/one"}},
            "hosts": {"remote": {"transport": "stdio", "program": "bin/two"}}
        }"#;
        assert_eq!(
            error(relative),
            ConfigError::RelativeProgram {
                agent: "remote".to_owned(),
                program: "bin/two".to_owned(),
            }
        );
        let cleartext = r#"{
            "agents": {"one": {"transport": "stdio", "program": "/bin/one"}},
            "hosts": {"remote": {"transport": "websocket", "url": "ws://remote.example/acp"}}
        }"#;
        assert_eq!(
            error(cleartext),
            ConfigError::InsecureUrl {
                agent: "remote".to_owned(),
            }
        );
    }

    #[test]
    fn debug_redacts_every_credential() {
        let table = parse(VALID).expect("the table is valid");
        let debug = format!("{table:?}");
        assert!(!debug.contains("s3cret"), "{debug}");
        assert!(!debug.contains("token"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(debug.contains("wss://agents.example/acp"), "{debug}");
    }

    #[test]
    fn an_invalid_url_error_names_only_the_agent() {
        // The variant holds no URL, so no credential a URL might carry can be printed.
        let error = ConfigError::InvalidUrl {
            agent: "one".to_owned(),
        };
        assert_eq!(
            error.to_string(),
            "agent \"one\" url must start with wss:// (or ws:// on loopback)"
        );
        assert_eq!(format!("{error:?}"), "InvalidUrl { agent: \"one\" }");
    }

    #[test]
    fn a_table_round_trips_through_json() {
        let table = parse(VALID).expect("the table is valid");
        let text = serde_json::to_string(&table).expect("the table serializes");
        assert!(
            text.contains(r#""transport":"websocket""#),
            "the serialized tag is websocket: {text}"
        );
        let again = parse(&text).expect("the serialized table is valid");
        assert_eq!(again.names(), table.names());
        assert_eq!(again.default_name(), table.default_name());
        assert!(matches!(
            &again.resolve("remote").expect("remote exists").transport,
            AgentTransport::WebSocket { url, secret, bearer }
                if url == "wss://agents.example/acp"
                    && secret.as_deref() == Some("s3cret")
                    && bearer.as_deref() == Some("token")
        ));
    }

    #[test]
    fn every_error_has_a_message() {
        let cases = [
            (ConfigError::Json("boom".to_owned()), "boom"),
            (ConfigError::NoAgents, "empty"),
            (ConfigError::InvalidName("Agent".to_owned()), "Agent"),
            (ConfigError::InvalidHost("Bad".to_owned()), "Bad"),
            (
                ConfigError::RelativeProgram {
                    agent: "one".to_owned(),
                    program: "bin".to_owned(),
                },
                "absolute",
            ),
            (
                ConfigError::InvalidUrl {
                    agent: "one".to_owned(),
                },
                "wss://",
            ),
            (
                ConfigError::InsecureUrl {
                    agent: "one".to_owned(),
                },
                "wss://",
            ),
            (ConfigError::UnknownAgent("two".to_owned()), "two"),
            (ConfigError::UnknownHost("far".to_owned()), "far"),
            (ConfigError::NoDefaultAgent, "default"),
        ];
        for (error, expected) in cases {
            assert!(error.to_string().contains(expected), "{error}");
        }
    }
}
