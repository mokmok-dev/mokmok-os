//! The durable child session a parent turn may delegate to.

use std::future::Future;

use mokmok_protocol::{ExecutionID, SessionID, TurnID};
use uuid::Uuid;

use crate::error::Error;
use crate::model::Usage;
use crate::task::{Budget, Task};

/// How many bytes of one subagent goal the log may hold.
///
/// A goal is clipped rather than refused. It is the exact phrase the child receives, and the
/// parent model's opening words are the words it meant.
pub const MAX_SUBAGENT_GOAL_BYTES: usize = 2048;

/// How many turns a child session is allowed at most, so a deep parent does not hand the
/// whole budget down and lose the framing that made the child small.
pub const CHILD_TURNS: u32 = 8;

/// What a parent turn asks of a child session.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SubagentCall {
    /// The parent session that launched the child.
    pub session: SessionID,
    /// The parent turn that asked for the child.
    pub turn: TurnID,
    /// The derived execution this delegation owns.
    pub execution: ExecutionID,
    /// The goal handed to the child, already clipped.
    pub goal: String,
}

/// What a child session finally answered, with the tokens it spent to get there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentOutcome {
    pub reply: String,
    pub usage: Usage,
}

/// The port a parent session uses to run a child session.
pub trait Subagents: Sync {
    /// Run the child session named by `call` to completion and return its final reply.
    fn run(
        &self,
        call: &SubagentCall,
    ) -> impl Future<Output = Result<SubagentOutcome, Error>>;
}

/// The deterministic child session identity for one parent execution.
///
/// A crash between the parent's intent and outcome must reopen the same child stream, not spawn
/// a second one, so this id derives from the durable execution rather than a fresh uuid.
#[must_use]
pub fn child_session(execution: ExecutionID) -> SessionID {
    SessionID::from_uuid(Uuid::new_v5(&execution.as_uuid(), b"subagent"))
}

/// The head of `goal`, clipped to [`MAX_SUBAGENT_GOAL_BYTES`] without splitting a character.
#[must_use]
pub fn clip_goal(goal: &str) -> String {
    crate::record::clipped(goal, MAX_SUBAGENT_GOAL_BYTES)
}

/// The budget a child session receives from its parent's budget.
#[must_use]
pub fn child_budget(parent: &Budget) -> Budget {
    Budget {
        turns: parent.turns.min(CHILD_TURNS),
        attempts: parent.attempts,
        tokens: parent.tokens / 2,
        observation_bytes: parent.observation_bytes,
        context_turns: parent.context_turns,
        context_bytes: parent.context_bytes,
    }
}

/// Build the child task from the recorded parent task.
///
/// The child inherits the parent's grants, checkout, program, deadline, search roots, and
/// skills, but delegation is turned off so a child cannot delegate further.
#[must_use]
pub fn child_task(
    parent: &Task,
    goal: &str,
) -> Task {
    let mut setting = parent.setting.clone();
    setting.delegate = false;

    Task {
        goal: goal.to_owned(),
        setting,
        budget: child_budget(&parent.budget),
    }
}

#[cfg(test)]
mod tests {
    use super::{CHILD_TURNS, MAX_SUBAGENT_GOAL_BYTES, child_budget, child_session, clip_goal};
    use crate::task::Budget;
    use crate::test_support::{execution_id, task, turn_id};

    #[test]
    fn child_session_derives_from_the_execution() {
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let child = child_session(execution);

        assert_eq!(
            child.as_uuid(),
            uuid::Uuid::new_v5(&execution.as_uuid(), b"subagent"),
        );
    }

    #[test]
    fn clip_goal_keeps_the_head_under_the_limit() {
        let long = "g".repeat(MAX_SUBAGENT_GOAL_BYTES + 10);
        let clipped = clip_goal(&long);

        assert_eq!(clipped.len(), MAX_SUBAGENT_GOAL_BYTES);
        assert!(long.starts_with(&clipped));
    }

    #[test]
    fn child_budget_caps_turns_preserves_zero_and_halves_tokens() {
        let parent = Budget {
            turns: 100,
            attempts: 5,
            tokens: 1000,
            observation_bytes: 4096,
            context_turns: 8,
            context_bytes: 65_536,
        };
        let child = child_budget(&parent);

        assert_eq!(child.turns, CHILD_TURNS);
        assert_eq!(child.attempts, 5);
        assert_eq!(child.tokens, 500);
        assert_eq!(child.observation_bytes, 4096);
        assert_eq!(child.context_turns, 8);
        assert_eq!(child.context_bytes, 65_536);
    }

    #[test]
    fn child_budget_preserves_zero_turns_and_tokens() {
        let parent = Budget {
            turns: 0,
            attempts: 3,
            tokens: 0,
            observation_bytes: 0,
            context_turns: 0,
            context_bytes: 0,
        };
        let child = child_budget(&parent);

        assert_eq!(child.turns, 0);
        assert_eq!(child.tokens, 0);
    }

    #[test]
    fn child_task_turns_delegation_off() {
        let parent = task(4);
        let child = super::child_task(&parent, "do this");

        assert_eq!(child.goal, "do this");
        assert!(!child.setting.delegate);
        assert_eq!(child.budget.turns, parent.budget.turns.min(CHILD_TURNS));
    }
}
