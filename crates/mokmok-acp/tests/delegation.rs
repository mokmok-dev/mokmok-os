//! The one-shot delegation entry point.

mod common;

use common::{Recorder, TestResult, within_deadline};
use mokmok_acp::{ConnectionConfig, StdioConfig, StopReason, Task, delegate_task};

#[test]
fn delegate_task_opens_runs_and_closes() -> TestResult {
    within_deadline(|| {
        let config = ConnectionConfig::Stdio(StdioConfig::new(env!(
            "CARGO_BIN_EXE_mokmok-acp-test-agent"
        )));
        let cwd = std::env::current_dir()?;
        let task = Task::new(&cwd, "do the task");
        let mut handler = Recorder::default();
        let turn = delegate_task(config, &task, &mut handler)?;
        assert_eq!(turn.stop_reason, StopReason::EndTurn);
        assert_eq!(handler.updates, 1);
        assert_eq!(task.prompt, "do the task");
        Ok(())
    })
}
