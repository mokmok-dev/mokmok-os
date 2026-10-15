//! The bytes the model is handed: a system prompt, the goal, and the turns it may see.

use crate::dialogue::Transcript;
use crate::task::Task;
use crate::tool::clip;
use mokmok_protocol::TurnID;
use mokmok_state_machine::{Turn, TurnNumber};

/// The canonical prompt content for one model call.
///
/// It carries no attempt number, step id, or clock, so a re-ask after a crash rebuilds byte-for-byte
/// the content the first attempt sent.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Context {
    pub system: String,
    pub goal: String,
    pub turns: Vec<ContextTurn>,
    /// Whether the session holds a granted root. The body offers the finder exactly when the
    /// prompt names it, because both are built from the same recorded task.
    #[serde(default)]
    pub search: bool,
}

/// One turn the model may see: what opened it and what the agent did with it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ContextTurn {
    pub number: TurnNumber,
    /// What the human said to open this turn, when a human opened it.
    ///
    /// Absent on a turn the agent opened itself — the ask that follows a tool call in the same
    /// exchange — so a reader can tell a new human message from a continuation of the agent's work.
    pub speech: Option<String>,
    /// What the agent did in this turn, once the turn settled.
    ///
    /// Absent on the turn being asked now, which is how the human's newest line reaches the model:
    /// the line opens the very turn that answers it, so that turn is unsettled while it is asked.
    pub act: Option<ContextAct>,
}

/// What the agent did in one settled turn.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "act", rename_all = "snake_case")]
pub enum ContextAct {
    /// The agent asked for the one tool, and it ran.
    Ran {
        command: String,
        observation: String,
    },
    /// The agent answered without the tool.
    Said { reply: String },
    /// The agent asked the finder, and it answered.
    Found { query: String, observation: String },
}

impl Context {
    /// How many bytes this context spends, the measure [`Budget::context_bytes`] caps.
    ///
    /// [`Budget::context_bytes`]: crate::task::Budget::context_bytes
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.system.len()
            + self.goal.len()
            + self
                .turns
                .iter()
                .map(|turn| {
                    turn.speech.as_ref().map_or(0, String::len)
                        + match &turn.act {
                            Some(
                                ContextAct::Ran {
                                    command,
                                    observation,
                                }
                                | ContextAct::Found {
                                    query: command,
                                    observation,
                                },
                            ) => command.len() + observation.len(),
                            Some(ContextAct::Said { reply }) => reply.len(),
                            None => 0,
                        }
                        + 8
                })
                .sum::<usize>()
    }
}

/// The facts the model cannot infer.
///
/// The goal is deliberately absent: the adapter hands it in the user message, because an endpoint
/// measured here answers a body whose messages are the system prompt alone with nothing.
#[must_use]
pub fn system_prompt(task: &Task) -> String {
    // A session granted no root is handed the words it was handed before the finder existed. The
    // task is durable, so a resumed session must rebuild its first prompt byte for byte.
    let tooling = if task.setting.search.is_empty() {
        "You have one tool, run_shell, whose only argument is a command string; it runs `sh -c`.\n"
            .to_owned()
    } else {
        format!(
            "You have two tools. run_shell takes a command string and runs `sh -c`. find_files \
             takes a query string and searches the names and contents of the files under {}; it \
             enters no directory whose name begins with a dot, and it reads and changes nothing, so \
             a find never changes the checkout.\n",
            paths(&task.setting.search),
        )
    };

    format!(
        "You are a coding agent working in a scratch checkout.\n\
         {tooling}\
         The working directory is a fresh, empty, ephemeral directory, and stdin is empty.\n\
         There is no TCP network. Reach the checkout by absolute path, for example \
         `cd {checkout} && ...`.\n\
         The checkout grants allow what a command can reach: readable and executable paths are \
         {readable}; writable paths are {writable}. A command that reaches outside them is refused \
         rather than run, so work inside the grants.\n\
         When the goal is met, answer without calling the tool.",
        checkout = task.setting.checkout.display(),
        readable = paths(&task.setting.readable),
        writable = paths(&task.setting.writable),
    )
}

/// Names the call record a recovery never wrote, so a settled turn is shown rather than dropped.
const MISSING_COMMAND: &str = "(the command was not recorded)";

impl Transcript {
    /// Every turn the model may see, oldest first: a settled turn, or one holding the human line it
    /// is being asked to answer.
    ///
    /// A turn in flight with no speech contributes nothing, so a re-ask after a crash rebuilds the
    /// bytes the first attempt sent. A settled turn with no call record still appears, carrying the
    /// rendering the recovery settlement gives it, so an interrupted command stays visible.
    #[must_use]
    pub fn shown(&self) -> Vec<ContextTurn> {
        self.turns_in_order()
            .into_iter()
            .filter(|(turn, state)| state.outcome().is_some() || self.speeches.contains_key(turn))
            .map(|(turn, state)| ContextTurn {
                number: state.number().unwrap_or(TurnNumber::new(0)),
                speech: self.speech(turn).map(str::to_owned),
                act: self.act_of(turn, state),
            })
            .collect()
    }

    /// What the agent did in one turn, as the model sees it, or nothing while the turn is asked.
    fn act_of(
        &self,
        turn: TurnID,
        state: &Turn,
    ) -> Option<ContextAct> {
        state.outcome()?;

        if let Some(call) = self.call_of(turn) {
            return Some(ContextAct::Ran {
                command: call.command.clone(),
                observation: self.shown_observation(turn),
            });
        }

        if let Some(find) = self.find_of(turn) {
            return Some(ContextAct::Found {
                query: find.query.clone(),
                observation: clip(find.report.observation.as_bytes(), self.observation_limit()),
            });
        }

        if let Some(answer) = self.newest_answer(turn)
            && answer.action == crate::model::Action::Final
        {
            return Some(ContextAct::Said {
                reply: answer.prose.clone(),
            });
        }

        Some(ContextAct::Ran {
            command: MISSING_COMMAND.to_owned(),
            observation: self.shown_observation(turn),
        })
    }

    /// How many bytes of one observation the model is shown, as a `usize`.
    fn observation_limit(&self) -> usize {
        bytes_of(
            self.task
                .as_ref()
                .map_or(0, |task| task.budget.observation_bytes),
        )
    }

    fn shown_observation(
        &self,
        turn: TurnID,
    ) -> String {
        let limit = self.observation_limit();

        self.observation(turn)
            .map_or_else(String::new, |text| clip(text.as_bytes(), limit))
    }
}

/// The prompt content for the next model call, built from the fold.
///
/// A settled turn appears, and so does the turn holding the human's newest line while it is being
/// asked, so the line that opened a turn reaches the model as the prompt for that turn. An attempt
/// repeated under a new number rebuilds the same bytes. The newest
/// [`Budget::context_turns`](crate::task::Budget::context_turns) are kept, and the oldest are
/// dropped until the whole context fits [`Budget::context_bytes`](crate::task::Budget::context_bytes).
#[must_use]
pub fn context(transcript: &Transcript) -> Context {
    let Some(task) = transcript.task.as_ref() else {
        return Context {
            system: String::new(),
            goal: String::new(),
            turns: Vec::new(),
            search: false,
        };
    };

    let mut turns = transcript.shown();
    keep_newest(&mut turns, task.budget.context_turns as usize);

    let mut context = Context {
        system: system_prompt(task),
        goal: task.goal.clone(),
        turns,
        search: !task.setting.search.is_empty(),
    };
    while context.bytes() > bytes_of(task.budget.context_bytes) && !context.turns.is_empty() {
        context.turns.remove(0);
    }

    context
}

/// A byte budget as a `usize`, treating zero as "no limit".
fn bytes_of(limit: u64) -> usize {
    if limit == 0 {
        usize::MAX
    } else {
        usize::try_from(limit).unwrap_or(usize::MAX)
    }
}

fn keep_newest(
    turns: &mut Vec<ContextTurn>,
    keep: usize,
) {
    if keep != 0 && turns.len() > keep {
        turns.drain(..turns.len() - keep);
    }
}

fn paths(granted: &[std::path::PathBuf]) -> String {
    if granted.is_empty() {
        return "none".to_owned();
    }

    granted
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::{
        Context, ContextAct, ContextTurn, MISSING_COMMAND, bytes_of, context, keep_newest, paths,
        system_prompt,
    };
    use crate::dialogue::Transcript;
    use crate::find::FindReport;
    use crate::model::Action;
    use crate::record::{CallRecord, Record};
    use crate::test_support::{
        act_intent_records, act_outcome_records, answer_records, apply_all, artifact,
        begin_records, execution_id, find_records, open_records, session, speech_records, task,
        turn_id,
    };
    use crate::tool::{Exit, ToolReport, UNKNOWN_WARNING, observation};
    use mokmok_protocol::{ExecutionID, TurnID};
    use mokmok_state_machine::{
        Attempt, Deadline, Fact, Resolution, TurnFact, TurnNumber, TurnOutcome,
    };

    fn turn_of(number: u64) -> ContextTurn {
        ContextTurn {
            number: TurnNumber::new(number),
            speech: None,
            act: Some(ContextAct::Ran {
                command: format!("command-{number}"),
                observation: format!("observation-{number}"),
            }),
        }
    }

    fn ran_command(turn: &ContextTurn) -> &str {
        match &turn.act {
            Some(ContextAct::Ran { command, .. }) => command,
            other => panic!("expected a ran act, got {other:?}"),
        }
    }

    fn ran_observation(turn: &ContextTurn) -> &str {
        match &turn.act {
            Some(ContextAct::Ran { observation, .. }) => observation,
            other => panic!("expected a ran act, got {other:?}"),
        }
    }

    /// A transcript with `count` settled turns, each showing a distinct command.
    fn settled_turns(
        task: &crate::task::Task,
        count: u64,
    ) -> Transcript {
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(task));
        for number in 1..=count {
            let turn = turn_id(number);
            let execution = execution_id(turn);
            let command = format!("command-{number}");
            apply_all(&mut transcript, &open_records(turn, number, 1));
            apply_all(
                &mut transcript,
                &answer_records(
                    turn,
                    1,
                    Action::Command {
                        command: command.clone(),
                    },
                ),
            );
            apply_all(
                &mut transcript,
                &act_intent_records(turn, execution, &command),
            );
            apply_all(
                &mut transcript,
                &act_outcome_records(turn, execution, Exit::Finished { code: 0 }, &[]),
            );
        }
        transcript
    }

    /// The facts recovery writes for one interrupted execution, and nothing else.
    fn recovery_facts(
        turn: TurnID,
        execution: ExecutionID,
    ) -> Vec<Record> {
        vec![
            Record::Session {
                fact: Fact::ExecutionSettled {
                    execution,
                    resolution: Resolution::Unknown,
                },
            },
            Record::Turn {
                turn,
                fact: TurnFact::ToolResolved {
                    execution,
                    resolution: Resolution::Unknown,
                },
            },
            Record::Turn {
                turn,
                fact: TurnFact::TurnSettled {
                    outcome: TurnOutcome::Failed,
                },
            },
        ]
    }

    /// The turn facts a crash leaves in flight, so a call names a requested execution.
    fn in_flight_turn(
        turn: TurnID,
        execution: ExecutionID,
    ) -> Vec<Record> {
        vec![
            Record::Turn {
                turn,
                fact: TurnFact::TurnOpened {
                    number: TurnNumber::new(1),
                },
            },
            Record::Turn {
                turn,
                fact: TurnFact::ModelRequested {
                    attempt: Attempt::new(1),
                },
            },
            Record::Turn {
                turn,
                fact: TurnFact::ModelAnswered,
            },
            Record::Session {
                fact: Fact::ExecutionLaunched {
                    execution,
                    deadline: Deadline::Bounded,
                },
            },
            Record::Turn {
                turn,
                fact: TurnFact::ToolRequested {
                    execution,
                    deadline: Deadline::Bounded,
                },
            },
        ]
    }

    #[test]
    fn an_unknown_outcome_renders_the_warning() {
        assert_eq!(observation(Resolution::Unknown, &[], &[]), UNKNOWN_WARNING);
    }

    #[test]
    fn context_bytes_counts_the_system_the_goal_and_every_turn() {
        let built = Context {
            system: "sys".to_owned(),
            goal: "goal!!".to_owned(),
            turns: vec![turn_of(1)],
            search: false,
        };

        assert_eq!(
            built.bytes(),
            3 + 6 + ("command-1".len() + "observation-1".len() + 8)
        );
    }

    #[test]
    fn the_system_prompt_states_the_role_the_grants_and_the_limits() {
        let prompt = system_prompt(&task(4));

        assert!(prompt.contains("coding agent"), "{prompt}");
        assert!(prompt.contains("run_shell"), "{prompt}");
        assert!(prompt.contains("There is no TCP network"), "{prompt}");
        assert!(prompt.contains("stdin is empty"), "{prompt}");
        assert!(prompt.contains("/nix/store"), "{prompt}");
    }

    #[test]
    fn the_goal_rides_the_context_and_not_the_system_prompt() {
        let task = task(4);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));

        let built = context(&transcript);

        assert_eq!(built.goal, "make the failing test pass");
        assert!(!built.system.contains("make the failing test pass"));
    }

    #[test]
    fn paths_names_no_grants_and_joins_the_rest() {
        assert_eq!(paths(&[]), "none");
        assert_eq!(
            paths(&["/nix/store".into(), "/srv/checkout".into()]),
            "/nix/store, /srv/checkout",
        );
    }

    #[test]
    fn bytes_of_treats_zero_as_no_limit() {
        assert_eq!(bytes_of(0), usize::MAX);
        assert_eq!(bytes_of(7), 7);
    }

    #[test]
    fn keep_newest_keeps_the_last_and_zero_keeps_all() {
        let mut trimmed = vec![turn_of(1), turn_of(2), turn_of(3)];
        keep_newest(&mut trimmed, 1);
        assert_eq!(trimmed.len(), 1);
        assert_eq!(ran_command(&trimmed[0]), "command-3");

        let mut all = vec![turn_of(1), turn_of(2), turn_of(3)];
        keep_newest(&mut all, 0);
        // A `len > keep` widened to `len >= keep` is indistinguishable here: at `len == keep` the
        // drain is `..0`, which removes nothing.
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn a_context_over_the_byte_budget_drops_the_oldest_turns() {
        let task = task(4);
        let mut transcript = settled_turns(&task, 3);
        let full = context(&transcript);
        assert_eq!(full.turns.len(), 3);

        let limit = u64::try_from(full.bytes()).unwrap();
        transcript.task.as_mut().unwrap().budget.context_bytes = limit - 1;
        let built = context(&transcript);

        assert_eq!(built.turns.len(), 2);
        assert_eq!(ran_command(&built.turns[0]), "command-2");
        assert_eq!(ran_command(&built.turns[1]), "command-3");
    }

    #[test]
    fn a_context_exactly_at_the_byte_budget_keeps_every_turn() {
        let task = task(4);
        let mut transcript = settled_turns(&task, 3);
        let full = context(&transcript);

        transcript.task.as_mut().unwrap().budget.context_bytes =
            u64::try_from(full.bytes()).unwrap();
        let built = context(&transcript);

        assert_eq!(built.turns.len(), 3);
        assert_eq!(ran_command(&built.turns[0]), "command-1");
    }

    #[test]
    fn a_context_budget_below_the_system_prompt_drops_every_turn() {
        let task = task(4);
        let mut transcript = settled_turns(&task, 3);
        transcript.task.as_mut().unwrap().budget.context_bytes = 1;

        let built = context(&transcript);

        assert_eq!(built.turns, Vec::new());
        assert_eq!(built.system, system_prompt(&task));
    }

    #[test]
    fn a_settled_turn_without_a_call_record_is_still_shown() {
        let task = task(4);
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        apply_all(&mut transcript, &in_flight_turn(turn, execution));
        apply_all(&mut transcript, &recovery_facts(turn, execution));

        let built = context(&transcript);

        assert_eq!(built.turns.len(), 1);
        assert_eq!(ran_command(&built.turns[0]), MISSING_COMMAND);
        assert_eq!(ran_observation(&built.turns[0]), UNKNOWN_WARNING);
    }

    #[test]
    fn a_recovery_shaped_history_carries_the_unknown_warning() {
        let task = task(4);
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut transcript = Transcript::new(session());
        // The task record predates the crash; seeding the fold for it keeps this history to machine
        // facts plus the call, which is the shape recovery reads.
        transcript.task = Some(task);

        apply_all(
            &mut transcript,
            &[Record::Session {
                fact: Fact::SessionOpened,
            }],
        );
        apply_all(&mut transcript, &in_flight_turn(turn, execution));
        apply_all(
            &mut transcript,
            &[Record::Call(CallRecord {
                turn,
                execution,
                command: "cargo test".to_owned(),
            })],
        );
        apply_all(&mut transcript, &recovery_facts(turn, execution));

        let built = context(&transcript);

        assert_eq!(built.turns.len(), 1);
        assert_eq!(ran_command(&built.turns[0]), "cargo test");
        assert_eq!(ran_observation(&built.turns[0]), UNKNOWN_WARNING);
    }

    #[test]
    fn a_later_context_carries_the_unknown_warning() {
        let task = task(4);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        let turn = turn_id(1);
        apply_all(&mut transcript, &open_records(turn, 1, 1));
        apply_all(
            &mut transcript,
            &answer_records(
                turn,
                1,
                Action::Command {
                    command: "rm -rf build".to_owned(),
                },
            ),
        );
        let execution = execution_id(turn);
        apply_all(
            &mut transcript,
            &act_intent_records(turn, execution, "rm -rf build"),
        );
        apply_all(
            &mut transcript,
            &act_outcome_records(turn, execution, Exit::Unknown, b""),
        );

        let built = context(&transcript);
        assert_eq!(built.turns.len(), 1);
        assert_eq!(ran_observation(&built.turns[0]), UNKNOWN_WARNING);
    }

    #[test]
    fn a_recorded_report_observation_reaches_the_next_context_unchanged() {
        let task = task(4);
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        apply_all(&mut transcript, &open_records(turn, 1, 1));
        apply_all(
            &mut transcript,
            &answer_records(
                turn,
                1,
                Action::Command {
                    command: "ls".to_owned(),
                },
            ),
        );
        apply_all(&mut transcript, &act_intent_records(turn, execution, "ls"));
        // The rendering is deliberately not what the settlement facts alone would derive, so a fold
        // that discarded the field and re-rendered would not produce it.
        apply_all(
            &mut transcript,
            &[
                Record::Session {
                    fact: Fact::ExecutionSettled {
                        execution,
                        resolution: Resolution::Finished { code: 0 },
                    },
                },
                Record::Session {
                    fact: Fact::ResultApplied { execution },
                },
                Record::Result {
                    execution,
                    report: ToolReport {
                        exit: Exit::Finished { code: 0 },
                        attempted: true,
                        stdout: artifact(0x01),
                        stderr: artifact(0x02),
                        observation: "the tool's own rendering".to_owned(),
                    },
                },
                Record::Turn {
                    turn,
                    fact: TurnFact::ToolResolved {
                        execution,
                        resolution: Resolution::Finished { code: 0 },
                    },
                },
                Record::Turn {
                    turn,
                    fact: TurnFact::TurnSettled {
                        outcome: TurnOutcome::Completed,
                    },
                },
            ],
        );

        let built = context(&transcript);

        assert_eq!(built.turns.len(), 1);
        assert_eq!(ran_observation(&built.turns[0]), "the tool's own rendering");
    }

    #[test]
    fn a_settlement_without_a_report_renders_from_its_facts() {
        let task = task(4);
        let turn = turn_id(1);
        let execution = execution_id(turn);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        apply_all(&mut transcript, &in_flight_turn(turn, execution));
        apply_all(
            &mut transcript,
            &[
                Record::Session {
                    fact: Fact::ExecutionSettled {
                        execution,
                        resolution: Resolution::Finished { code: 7 },
                    },
                },
                Record::Session {
                    fact: Fact::ResultApplied { execution },
                },
                Record::Turn {
                    turn,
                    fact: TurnFact::ToolResolved {
                        execution,
                        resolution: Resolution::Finished { code: 7 },
                    },
                },
                Record::Turn {
                    turn,
                    fact: TurnFact::TurnSettled {
                        outcome: TurnOutcome::Completed,
                    },
                },
            ],
        );

        let built = context(&transcript);

        assert_eq!(built.turns.len(), 1);
        assert_eq!(ran_observation(&built.turns[0]), "exit code 7");
    }

    #[test]
    fn a_context_for_a_reask_is_byte_identical() {
        let task = task(4);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        let turn = turn_id(1);
        apply_all(&mut transcript, &open_records(turn, 1, 1));

        let first = context(&transcript);
        apply_all(&mut transcript, &open_records(turn, 1, 2));
        let second = context(&transcript);

        assert_eq!(first, second);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap(),
        );
    }

    #[test]
    fn a_settled_turn_is_shown_and_the_turn_in_flight_is_not() {
        let task = task(4);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        let first_turn = turn_id(1);
        let execution = execution_id(first_turn);
        apply_all(&mut transcript, &open_records(first_turn, 1, 1));
        apply_all(
            &mut transcript,
            &answer_records(
                first_turn,
                1,
                Action::Command {
                    command: "ls".to_owned(),
                },
            ),
        );
        apply_all(
            &mut transcript,
            &act_intent_records(first_turn, execution, "ls"),
        );
        apply_all(
            &mut transcript,
            &act_outcome_records(first_turn, execution, Exit::Finished { code: 0 }, b"a\nb"),
        );
        let second_turn = turn_id(2);
        apply_all(&mut transcript, &open_records(second_turn, 2, 1));

        let built: Context = context(&transcript);
        assert_eq!(built.turns.len(), 1);
        assert_eq!(ran_command(&built.turns[0]), "ls");
    }

    #[test]
    fn the_turn_a_line_opened_is_shown_to_the_model_while_it_is_asked() {
        let task = task(4);
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        apply_all(&mut transcript, &speech_records(turn, 1, "hello"));

        let built = context(&transcript);

        assert_eq!(built.turns.len(), 1);
        assert_eq!(built.turns[0].number, TurnNumber::new(1));
        assert_eq!(built.turns[0].speech.as_deref(), Some("hello"));
        assert_eq!(built.turns[0].act, None);
    }

    #[test]
    fn a_re_ask_with_a_line_still_rebuilds_the_bytes_the_first_attempt_sent() {
        let task = task(4);
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        apply_all(&mut transcript, &speech_records(turn, 1, "hello"));
        apply_all(&mut transcript, &open_records(turn, 1, 1));

        let first = context(&transcript);
        assert_eq!(first.turns[0].speech.as_deref(), Some("hello"));

        apply_all(&mut transcript, &open_records(turn, 1, 2));
        let second = context(&transcript);

        assert_eq!(first, second);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap(),
        );
    }

    #[test]
    fn a_malformed_answer_still_renders_its_turn() {
        let task = task(4);
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));
        apply_all(&mut transcript, &open_records(turn, 1, 1));
        apply_all(
            &mut transcript,
            &answer_records(
                turn,
                1,
                Action::Malformed {
                    reason: "the tool arguments are not json".to_owned(),
                },
            ),
        );
        apply_all(
            &mut transcript,
            &crate::test_support::close_records(turn, false, None),
        );

        let built = context(&transcript);

        assert_eq!(built.turns.len(), 1);
        assert_eq!(built.turns[0].speech, None);
        assert_eq!(ran_command(&built.turns[0]), MISSING_COMMAND);
    }

    /// The system prompt this crate produced before the finder existed, word for word.
    const PROMPT_WITHOUT_FINDER: &str = "You are a coding agent working in a scratch checkout.\n\
         You have one tool, run_shell, whose only argument is a command string; it runs `sh -c`.\n\
         The working directory is a fresh, empty, ephemeral directory, and stdin is empty.\n\
         There is no TCP network. Reach the checkout by absolute path, for example \
         `cd /srv/checkout && ...`.\n\
         The checkout grants allow what a command can reach: readable and executable paths are \
         /nix/store; writable paths are /srv/checkout. A command that reaches outside them is refused \
         rather than run, so work inside the grants.\n\
         When the goal is met, answer without calling the tool.";

    /// The system prompt a session with a granted root is handed, word for word.
    const PROMPT_WITH_FINDER: &str = "You are a coding agent working in a scratch checkout.\n\
         You have two tools. run_shell takes a command string and runs `sh -c`. find_files takes a \
         query string and searches the names and contents of the files under /srv/checkout; it \
         enters no directory whose name begins with a dot, and it reads and changes nothing, so a \
         find never changes the checkout.\n\
         The working directory is a fresh, empty, ephemeral directory, and stdin is empty.\n\
         There is no TCP network. Reach the checkout by absolute path, for example \
         `cd /srv/checkout && ...`.\n\
         The checkout grants allow what a command can reach: readable and executable paths are \
         /nix/store; writable paths are /srv/checkout. A command that reaches outside them is refused \
         rather than run, so work inside the grants.\n\
         When the goal is met, answer without calling the tool.";

    /// A transcript whose first turn asked the finder and settled on what it answered.
    fn found_turn(
        task: &crate::task::Task,
        query: &str,
        observation: &str,
    ) -> Transcript {
        let turn = turn_id(1);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(task));
        apply_all(&mut transcript, &open_records(turn, 1, 1));
        apply_all(
            &mut transcript,
            &answer_records(
                turn,
                1,
                Action::Find {
                    query: query.to_owned(),
                },
            ),
        );
        apply_all(
            &mut transcript,
            &find_records(
                turn,
                query,
                &FindReport {
                    observation: observation.to_owned(),
                },
            ),
        );
        transcript
    }

    #[test]
    fn a_task_with_no_granted_root_gets_the_prompt_it_got_before_the_finder() {
        let mut task = task(4);
        task.setting.search.clear();

        assert_eq!(system_prompt(&task), PROMPT_WITHOUT_FINDER);
    }

    #[test]
    fn a_task_with_a_granted_root_is_offered_the_finder_over_that_root() {
        assert_eq!(system_prompt(&task(4)), PROMPT_WITH_FINDER);
    }

    #[test]
    fn the_context_says_whether_the_task_granted_a_root() {
        let task = task(4);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&task));

        assert!(context(&transcript).search);

        let mut without = task;
        without.setting.search.clear();
        assert!(!context(&found_turn(&without, "bm25", "indexed=831 hits=0")).search);
    }

    #[test]
    fn a_context_written_before_the_finder_decodes_without_one() {
        let json = r#"{"system":"sys","goal":"goal","turns":[]}"#;

        assert_eq!(
            serde_json::from_str::<Context>(json).unwrap(),
            Context {
                system: "sys".to_owned(),
                goal: "goal".to_owned(),
                turns: Vec::new(),
                search: false,
            },
        );
    }

    #[test]
    fn a_find_is_shown_as_a_found_act_and_a_re_ask_rebuilds_the_same_bytes() {
        let task = task(4);
        let mut transcript = found_turn(&task, "bm25", "indexed=831 hits=2");
        apply_all(&mut transcript, &open_records(turn_id(2), 2, 1));

        let built = context(&transcript);

        assert_eq!(
            built.turns[0].act,
            Some(ContextAct::Found {
                query: "bm25".to_owned(),
                observation: "indexed=831 hits=2".to_owned(),
            }),
        );

        let first = serde_json::to_vec(&context(&transcript)).unwrap();
        apply_all(&mut transcript, &open_records(turn_id(2), 2, 2));
        let second = serde_json::to_vec(&context(&transcript)).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn a_find_observation_is_clipped_like_a_run_observation() {
        let mut task = task(4);
        task.budget.observation_bytes = 10;
        let mut transcript = found_turn(&task, "bm25", &"x".repeat(40));
        apply_all(&mut transcript, &open_records(turn_id(2), 2, 1));

        let built = context(&transcript);

        assert_eq!(
            built.turns[0].act,
            Some(ContextAct::Found {
                query: "bm25".to_owned(),
                observation: format!("{}\n… 30 bytes elided …\n{}", "x".repeat(5), "x".repeat(5)),
            }),
        );
    }
}
