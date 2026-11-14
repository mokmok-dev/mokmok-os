//! The runner that owns a table of both ACP agents and TypeScript plugins.

use mokmok_control_core::TaskSpec;

use crate::plugin::PluginRunner;
use crate::runner::{AcpRunner, AgentTable, AgentTransport, resolve_transport};
use crate::supervisor::{Reporter, RunOutcome, Runner};

/// Runs a task on whichever runner the table's transport names.
///
/// Each inner runner resolves the entry again. Resolution is one map lookup, so paying it
/// twice is cheaper than a shared resolved-config type that every caller would thread
/// through the [`Runner`] trait.
pub struct AgentRunner {
    table: AgentTable,
    acp: AcpRunner,
    plugins: PluginRunner,
}

impl AgentRunner {
    #[must_use]
    pub const fn new(
        table: AgentTable,
        acp: AcpRunner,
        plugins: PluginRunner,
    ) -> Self {
        Self {
            table,
            acp,
            plugins,
        }
    }

    #[must_use]
    pub const fn table(&self) -> &AgentTable {
        &self.table
    }
}

impl Runner for AgentRunner {
    fn run(
        &self,
        spec: &TaskSpec,
        reporter: &dyn Reporter,
    ) -> RunOutcome {
        match resolve_transport(&self.table, spec) {
            Ok(AgentTransport::Plugin { .. }) => self.plugins.run(spec, reporter),
            // A resolution failure is the ACP runner's to report with its own wording, so
            // one table cannot have two different messages for the same bad entry.
            Ok(_) | Err(_) => self.acp.run(spec, reporter),
        }
    }
}

impl std::fmt::Debug for AgentRunner {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        formatter
            .debug_struct("AgentRunner")
            .field("table", &self.table)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use mokmok_journal::JournalEvent;

    use super::*;
    use crate::plugin::PluginHost;
    use crate::runner::AgentConfig;
    use crate::supervisor::Reporter;

    fn table(transport: AgentTransport) -> AgentTable {
        AgentTable {
            agents: [("one".to_owned(), AgentConfig { transport })]
                .into_iter()
                .collect(),
            hosts: BTreeMap::new(),
            default: Some("one".to_owned()),
        }
    }

    fn spec(agent: &str) -> TaskSpec {
        TaskSpec {
            task_id: mokmok_control_core::TaskId::from_u128(1),
            title: "Say hello".to_owned(),
            instruction: "Greet the user.".to_owned(),
            agent: agent.to_owned(),
            host: None,
            workspace: None,
        }
    }

    struct Recorder {
        events: std::sync::Mutex<Vec<JournalEvent>>,
    }

    impl Reporter for Recorder {
        fn started(
            &self,
            _session: &str,
        ) -> Result<(), String> {
            Ok(())
        }

        fn update(
            &self,
            event: JournalEvent,
        ) -> Result<(), String> {
            self.events
                .lock()
                .expect("the recorder is not poisoned")
                .push(event);
            Ok(())
        }

        fn cancelled(&self) -> bool {
            false
        }
    }

    fn runner() -> AgentRunner {
        let plugins = PluginRunner::new(
            table(AgentTransport::Plugin {
                entry: PathBuf::from("/nonexistent/plugin.ts"),
                timeout_secs: None,
            }),
            PluginHost::new("/nonexistent/node"),
        );
        AgentRunner::new(
            AgentTable {
                agents: [(
                    "one".to_owned(),
                    AgentConfig {
                        transport: AgentTransport::Stdio {
                            program: PathBuf::from("/nonexistent/agent"),
                            args: Vec::new(),
                        },
                    },
                )]
                .into_iter()
                .collect(),
                hosts: BTreeMap::new(),
                default: Some("one".to_owned()),
            },
            AcpRunner::new(
                AgentTable {
                    agents: [(
                        "one".to_owned(),
                        AgentConfig {
                            transport: AgentTransport::Stdio {
                                program: PathBuf::from("/nonexistent/agent"),
                                args: Vec::new(),
                            },
                        },
                    )]
                    .into_iter()
                    .collect(),
                    hosts: BTreeMap::new(),
                    default: Some("one".to_owned()),
                },
                std::time::Duration::from_secs(1),
            ),
            plugins,
        )
    }

    #[test]
    fn an_unknown_agent_goes_to_the_acp_runner() {
        let recorder = Recorder {
            events: std::sync::Mutex::new(Vec::new()),
        };
        let outcome = runner().run(&spec("missing"), &recorder);
        assert!(matches!(outcome, RunOutcome::Failed { .. }));
    }

    #[test]
    fn a_stdio_entry_goes_to_the_acp_runner() {
        let recorder = Recorder {
            events: std::sync::Mutex::new(Vec::new()),
        };
        let outcome = runner().run(&spec("one"), &recorder);
        assert!(matches!(outcome, RunOutcome::Failed { .. }));
        assert!(
            recorder
                .events
                .lock()
                .expect("the recorder is not poisoned")
                .is_empty()
        );
    }

    #[test]
    fn debugging_a_runner_names_its_table() {
        let text = format!("{:?}", runner());
        assert!(text.contains("AgentRunner"), "{text}");
        assert!(text.contains("one"), "{text}");
    }
}
