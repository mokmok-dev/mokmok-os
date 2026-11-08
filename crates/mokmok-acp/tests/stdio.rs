//! A task delegated over the agent's standard input and output.

mod common;

use common::{Recorder, TestResult, within_deadline};
use mokmok_acp::{
    AcpClient, AcpError, ConnectionConfig, ProtocolVersion, StdioConfig, StopReason, TransportError,
};

fn config() -> ConnectionConfig {
    ConnectionConfig::Stdio(StdioConfig::new(env!(
        "CARGO_BIN_EXE_mokmok-acp-test-agent"
    )))
}

#[test]
fn a_task_runs_over_stdio() -> TestResult {
    within_deadline(|| {
        let mut client = AcpClient::open(config())?;
        assert_eq!(client.agent().protocol_version, ProtocolVersion::LATEST);
        let cwd = std::env::current_dir()?;
        let session = client.new_session(&cwd)?;
        assert_eq!(session.id().to_string(), "fake-session");
        assert_eq!(session.cwd(), cwd.as_path());

        let mut handler = Recorder::default();
        let turn = client.delegate(&session, "do the task", &mut handler)?;
        assert_eq!(turn.stop_reason, StopReason::EndTurn);
        assert_eq!(turn.session_id.to_string(), "fake-session");
        assert_eq!(handler.updates, 1);
        assert_eq!(handler.permissions, 1);

        client.close()?;
        assert!(matches!(
            client.cancel(&session),
            Err(AcpError::Transport(TransportError::Closed))
        ));
        client.close()?;
        Ok(())
    })
}
