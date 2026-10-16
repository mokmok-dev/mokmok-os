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
    /// Whether the session holds a writable checkout. The body offers the edit tool exactly when
    /// this flag is true, because both are built from the same recorded task.
    #[serde(default)]
    pub writable: bool,
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
    /// Absent in two cases. While the turn is being asked, which is how the human's newest line
    /// reaches the model: the line opens the very turn that answers it, so that turn is unsettled
    /// while it is asked. And on a settled turn the model never finished — one that requested no
    /// tool — where no command ran, so showing one would tell the model it had.
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
    Said {
        reply: String,
    },
    Found {
        query: String,
        observation: String,
    },
    /// The agent asked for an edit. The rendering names the file itself, so no path rides beside it.
    Edited {
        observation: String,
    },
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
                            Some(ContextAct::Edited { observation }) => observation.len(),
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
    // A tool this session cannot use is left out rather than described, because a body offering one
    // would leave the model asking into a refusal. Each condition here is the one the tool list and
    // the prompt are both built from, and the recorded task is durable, so a resumed session
    // rebuilds its first prompt byte for byte.
    let shell = "run_shell takes a command string and runs `sh -c`";
    let find = if task.setting.search.is_empty() {
        None
    } else {
        Some(format!(
            "find_files takes a query string and searches the names and contents of the files \
             under {}; it enters no directory whose name begins with a dot, and it reads and \
             changes nothing, so a find never changes the checkout",
            paths(&task.setting.search),
        ))
    };
    let edit = if task.setting.writable.is_empty() {
        None
    } else {
        Some(format!(
            "apply_patch takes a unified diff and rewrites one file under {}; one file per call, \
             and the patch applies cleanly or nothing is written",
            task.setting.checkout.display(),
        ))
    };

    let mut tools = vec![shell.to_owned()];
    tools.extend(find);
    tools.extend(edit);
    let tooling = if tools.is_empty() {
        String::new()
    } else {
        format!("You have these tools: {}.\n", tools.join("; "))
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
    /// is being asked to answer, each paired with the turn it names.
    ///
    /// A front end draws this rendering and must read the turn's own state beside it — its
    /// settlement, its attempts, and its answers — so the rendering carries the identity rather
    /// than a reader recovering it from the fold a second time.
    ///
    /// A turn in flight with no speech contributes nothing, so a re-ask after a crash rebuilds the
    /// bytes the first attempt sent. A settled turn with no call record still appears, carrying the
    /// rendering the recovery settlement gives it, so an interrupted command stays visible.
    #[must_use]
    pub fn shown_turns(&self) -> Vec<(TurnID, ContextTurn)> {
        self.turns_in_order()
            .into_iter()
            .filter(|(turn, state)| state.outcome().is_some() || self.speeches.contains_key(turn))
            .map(|(turn, state)| {
                let shown = ContextTurn {
                    number: state.number().unwrap_or(TurnNumber::new(0)),
                    speech: self.speech(turn).map(str::to_owned),
                    act: self.act_of(turn, state),
                };

                (turn, shown)
            })
            .collect()
    }

    /// The renderings [`shown_turns`] yields, without the turns that name them.
    ///
    /// [`shown_turns`]: Transcript::shown_turns
    #[must_use]
    pub fn shown(&self) -> Vec<ContextTurn> {
        self.shown_turns()
            .into_iter()
            .map(|(_, shown)| shown)
            .collect()
    }

    /// What the agent did in one turn, as the model sees it.
    ///
    /// Nothing while the turn is asked, and nothing on a settled turn that requested no tool: only
    /// a turn that dispatched a command is shown running one, so a turn the model never finished
    /// carries no act rather than a command that never ran.
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

        // A refusal decided before anything was written records an outcome and no intent, so the
        // outcome is what the turn is shown by, whether or not an intent stands beside it.
        let execution = crate::advance::execution_id(turn);
        if let Some(report) = self.edit_outcome_of(execution) {
            return Some(ContextAct::Edited {
                observation: clip(report.observation.as_bytes(), self.observation_limit()),
            });
        }

        if let Some(intent) = self.edit_of(turn) {
            return Some(ContextAct::Edited {
                observation: clip(
                    crate::edit::unproven(intent).as_bytes(),
                    self.observation_limit(),
                ),
            });
        }

        if let Some(answer) = self.newest_answer(turn)
            && answer.action == crate::model::Action::Final
        {
            return Some(ContextAct::Said {
                reply: answer.prose.clone(),
            });
        }

        // `MISSING_COMMAND` would tell the model a command ran, so a turn that requested no tool
        // shows nothing instead.
        state.tools().next()?;

        Some(ContextAct::Ran {
            command: MISSING_COMMAND.to_owned(),
            observation: self.shown_observation(turn),
        })
    }

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
            writable: false,
        };
    };

    let mut turns = transcript.shown();
    keep_newest(&mut turns, task.budget.context_turns as usize);

    let mut context = Context {
        system: system_prompt(task),
        goal: task.goal.clone(),
        turns,
        search: !task.setting.search.is_empty(),
        writable: !task.setting.writable.is_empty(),
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
    if keep == 0 {
        return;
    }
    // No `len > keep` guard: at `len == keep` the drain is `..0`, which removes nothing.
    let excess = turns.len().saturating_sub(keep);
    turns.drain(..excess);
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
    use crate::edit::EditOutcome;
    use crate::find::FindReport;
    use crate::model::{Action, Usage};
    use crate::record::{AnswerRecord, CallRecord, PromptRecord, Record, SpeechRecord};
    use crate::task::Task;
    use crate::test_support::{
        act_intent_records, act_outcome_records, answer_records, apply_all, artifact,
        begin_records, edit_intent_records, edit_outcome_records, edit_refusal_records,
        execution_id, find_records, open_records, session, speech_records, task, turn_id,
    };
    use crate::tool::{Exit, ToolReport, UNKNOWN_WARNING, observation};
    use mokmok_protocol::{ExecutionID, TurnID};
    use mokmok_state_machine::{
        Attempt, Deadline, Fact, Resolution, Turn, TurnFact, TurnNumber, TurnOutcome,
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
            writable: false,
        };

        assert_eq!(
            built.bytes(),
            3 + 6 + ("command-1".len() + "observation-1".len() + 8)
        );
    }

    #[test]
    fn context_flags_match_whether_search_and_write_roots_were_granted() {
        let writable = task(4);
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&writable));
        let built = context(&transcript);
        assert!(built.search, "search root granted");
        assert!(built.writable, "write root granted");

        let setting = crate::task::Setting {
            writable: Vec::new(),
            search: Vec::new(),
            ..writable.setting.clone()
        };
        let read_only = Task {
            setting,
            ..writable
        };
        let mut transcript = Transcript::new(session());
        apply_all(&mut transcript, &begin_records(&read_only));
        let built = context(&transcript);
        assert!(!built.search, "no search root");
        assert!(!built.writable, "no write root");
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
    fn a_settled_turn_the_model_never_finished_renders_no_act() {
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
            &[Record::Turn {
                turn,
                fact: TurnFact::TurnSettled {
                    outcome: TurnOutcome::Failed,
                },
            }],
        );

        let built = context(&transcript);

        assert_eq!(built.turns.len(), 1);
        assert_eq!(built.turns[0].speech, None);
        assert_eq!(built.turns[0].act, None);
    }

    #[test]
    fn shown_turns_pairs_every_rendering_with_the_turn_it_names() {
        let first = turn_id(1);
        let second = turn_id(2);
        let records = vec![
            Record::Session {
                fact: Fact::SessionOpened,
            },
            Record::Turn {
                turn: first,
                fact: TurnFact::TurnOpened {
                    number: TurnNumber::new(1),
                },
            },
            Record::Speech(SpeechRecord {
                turn: first,
                text: "hello".to_owned(),
            }),
            Record::Turn {
                turn: first,
                fact: TurnFact::ModelRequested {
                    attempt: Attempt::new(1),
                },
            },
            Record::Prompt(PromptRecord {
                turn: first,
                attempt: Attempt::new(1),
                context: artifact(0x11),
            }),
            Record::Turn {
                turn: first,
                fact: TurnFact::ModelAnswered,
            },
            Record::Answer(AnswerRecord {
                turn: first,
                attempt: Attempt::new(1),
                action: Action::Final,
                prose: "done".to_owned(),
                text: artifact(0x12),
                model: "test".to_owned(),
                usage: Usage {
                    input: 1,
                    output: 1,
                },
            }),
            Record::Turn {
                turn: first,
                fact: TurnFact::TurnSettled {
                    outcome: TurnOutcome::Completed,
                },
            },
            Record::Turn {
                turn: second,
                fact: TurnFact::TurnOpened {
                    number: TurnNumber::new(2),
                },
            },
            Record::Speech(SpeechRecord {
                turn: second,
                text: "again".to_owned(),
            }),
        ];

        let transcript = Transcript::replay(session(), &records).expect("the history folds");
        let paired = transcript.shown_turns();

        assert_eq!(
            paired,
            vec![
                (
                    first,
                    ContextTurn {
                        number: TurnNumber::new(1),
                        speech: Some("hello".to_owned()),
                        act: Some(ContextAct::Said {
                            reply: "done".to_owned(),
                        }),
                    },
                ),
                (
                    second,
                    ContextTurn {
                        number: TurnNumber::new(2),
                        speech: Some("again".to_owned()),
                        act: None,
                    },
                ),
            ],
        );
        assert_eq!(
            paired
                .iter()
                .map(|(_, turn)| turn.clone())
                .collect::<Vec<_>>(),
            transcript.shown(),
        );

        for (turn, shown) in paired {
            assert_eq!(
                Some(shown.number),
                transcript.turn(turn).and_then(Turn::number),
                "the identity must name the turn whose number the rendering carries",
            );
        }
    }

    const PROMPT_WITH_EDIT: &str = "You are a coding agent working in a scratch checkout.\n\
         You have these tools: run_shell takes a command string and runs `sh -c`; apply_patch \
         takes a unified diff and rewrites one file under /srv/checkout; one file per call, and the \
         patch applies cleanly or nothing is written.\n\
         The working directory is a fresh, empty, ephemeral directory, and stdin is empty.\n\
         There is no TCP network. Reach the checkout by absolute path, for example \
         `cd /srv/checkout && ...`.\n\
         The checkout grants allow what a command can reach: readable and executable paths are \
         /nix/store; writable paths are /srv/checkout. A command that reaches outside them is refused \
         rather than run, so work inside the grants.\n\
         When the goal is met, answer without calling the tool.";

    const PROMPT_WITH_EDIT_AND_FINDER: &str = "You are a coding agent working in a scratch checkout.\n\
         You have these tools: run_shell takes a command string and runs `sh -c`; find_files \
         takes a query string and searches the names and contents of the files under /srv/checkout; \
         it enters no directory whose name begins with a dot, and it reads and changes nothing, so a \
         find never changes the checkout; apply_patch takes a unified diff and rewrites one file \
         under /srv/checkout; one file per call, and the patch applies cleanly or nothing is \
         written.\n\
         The working directory is a fresh, empty, ephemeral directory, and stdin is empty.\n\
         There is no TCP network. Reach the checkout by absolute path, for example \
         `cd /srv/checkout && ...`.\n\
         The checkout grants allow what a command can reach: readable and executable paths are \
         /nix/store; writable paths are /srv/checkout. A command that reaches outside them is refused \
         rather than run, so work inside the grants.\n\
         When the goal is met, answer without calling the tool.";

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
    fn a_task_with_no_granted_root_but_a_writable_checkout_gets_the_edit_tool() {
        let mut task = task(4);
        task.setting.search.clear();

        assert_eq!(system_prompt(&task), PROMPT_WITH_EDIT);
    }

    #[test]
    fn a_task_with_no_writable_checkout_gets_only_the_shell_tool() {
        let mut task = task(4);
        task.setting.writable.clear();
        task.setting.search.clear();
        let prompt = system_prompt(&task);

        assert!(prompt.contains("run_shell"), "{prompt}");
        assert!(!prompt.contains("apply_patch"), "{prompt}");
        assert!(!prompt.contains("find_files"), "{prompt}");
        assert!(prompt.contains("writable paths are none"), "{prompt}");
    }

    #[test]
    fn a_task_with_a_granted_root_is_offered_the_finder_over_that_root() {
        let mut task = task(4);
        task.setting.writable.clear();
        let prompt = system_prompt(&task);

        assert!(prompt.contains("run_shell"), "{prompt}");
        assert!(prompt.contains("find_files"), "{prompt}");
        assert!(!prompt.contains("apply_patch"), "{prompt}");
    }

    #[test]
    fn a_task_with_both_tools_is_offered_both() {
        assert_eq!(system_prompt(&task(4)), PROMPT_WITH_EDIT_AND_FINDER);
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
                writable: false,
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

    #[test]
    fn a_recorded_edit_is_shown_as_an_edited_act() {
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
                Action::Edit {
                    patch: "p".to_owned(),
                    expect: None,
                },
            ),
        );
        apply_all(
            &mut transcript,
            &edit_intent_records(
                turn,
                execution,
                "p",
                "src/x.rs",
                &artifact(0x01),
                &artifact(0x02),
                "hunk 1 matched at line 1 (offset 0)",
            ),
        );
        apply_all(
            &mut transcript,
            &edit_outcome_records(turn, execution, EditOutcome::Applied, "edit applied"),
        );

        let built = context(&transcript);

        assert_eq!(
            built.turns[0].act,
            Some(ContextAct::Edited {
                observation: "edit applied".to_owned(),
            }),
        );
    }

    #[test]
    fn an_interrupted_edit_shows_the_unproven_warning() {
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
                Action::Edit {
                    patch: "p".to_owned(),
                    expect: None,
                },
            ),
        );
        apply_all(
            &mut transcript,
            &edit_intent_records(
                turn,
                execution,
                "p",
                "src/x.rs",
                &artifact(0x01),
                &artifact(0x02),
                "hunk 1 matched at line 1 (offset 0)",
            ),
        );
        // Recovery settles the turn failed, so the in-flight edit stays visible.
        apply_all(&mut transcript, &recovery_facts(turn, execution));

        let built = context(&transcript);

        assert_eq!(
            built.turns[0].act,
            Some(ContextAct::Edited {
                observation: crate::edit::unproven(transcript.edit_of(turn).unwrap(),),
            }),
        );
    }

    #[test]
    fn a_refused_edit_is_shown_by_its_outcome_rather_than_by_a_command() {
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
                Action::Edit {
                    patch: "p".to_owned(),
                    expect: None,
                },
            ),
        );
        apply_all(
            &mut transcript,
            &edit_refusal_records(
                turn,
                execution,
                "the patch names two files",
                "the edit was refused: the patch names two files",
            ),
        );

        let built = context(&transcript);

        assert_eq!(
            built.turns[0].act,
            Some(ContextAct::Edited {
                observation: "the edit was refused: the patch names two files".to_owned(),
            }),
        );
    }
}
