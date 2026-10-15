//! The pure core: two fields of state, three pure functions, one driver.

use std::mem;

use mokmok_agent_runtime::{
    Action, ContextAct, Halt, MAX_SPEECH_BYTES, Move, Progress, Transcript, next,
};
use mokmok_protocol::TurnID;
use mokmok_state_machine::{Attempt, Ending, Phase};

use crate::port::{Engine, Event, Key, Line, Screen, View};
use crate::{Error, Outcome};

/// What the conversation holds that the fold does not.
#[derive(Debug, Default)]
struct Ui {
    input: String,
    scroll: u16,
}

/// Whose turn it is, read off the fold.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// Nothing moves until a human line arrives.
    Waiting,
    /// The loop has something to do: a model call, a tool, or a settlement.
    Working,
    /// The session is closed and cannot take another line.
    Ended(Ending),
}

/// What one keystroke asked the shell to do.
#[derive(Debug, Eq, PartialEq)]
enum Reaction {
    /// Nothing beyond a redraw.
    Idle,
    /// The line the human submitted.
    Speak(String),
    /// Leave, with the session as it stands.
    Quit,
}

/// Whose turn it is. The one question the driver asks, and a pure function of the fold.
fn mode(fold: &Transcript) -> Mode {
    match fold.session.phase() {
        Phase::Closed(ending) => Mode::Ended(ending),
        // A cancel in force from another process: the loop settles what is in flight, then closes it.
        Phase::Draining => Mode::Working,
        // An unopened session is opened by its first line, not by a model call with nothing to answer.
        Phase::Unopened | Phase::Accepting => match next(fold) {
            Move::Begin { .. } | Move::Await => Mode::Waiting,
            _ => Mode::Working,
        },
    }
}

fn lines(fold: &Transcript) -> Vec<Line> {
    let mut lines = Vec::new();

    for (turn, shown) in fold.shown_turns() {
        if let Some(speech) = shown.speech {
            lines.push(Line::Human(speech));
        }

        match shown.act {
            Some(ContextAct::Ran {
                command,
                observation,
            }) => lines.push(Line::Work {
                command,
                observation,
            }),
            Some(ContextAct::Said { reply }) => lines.push(Line::Agent(reply)),
            Some(ContextAct::Found { query, observation }) => {
                lines.push(Line::Find { query, observation });
            },
            // A settled turn with no act is one the model never finished: the agent opened it, so
            // the human still needs to know it could not.
            None if fold
                .turn(turn)
                .is_some_and(|state| state.outcome().is_some()) =>
            {
                lines.push(Line::Trouble(reason(fold, turn)));
            },
            None => {},
        }
    }

    lines
}

/// Why a turn could not finish: what the model's unusable answer at the turn's highest attempt
/// said, or how many attempts it spent without answering.
fn reason(
    fold: &Transcript,
    turn: TurnID,
) -> String {
    let highest = fold.turn(turn).and_then(|state| state.attempts().max());

    let malformed = highest
        .and_then(|attempt| fold.answers.get(&(turn, attempt)))
        .and_then(|answer| match &answer.action {
            Action::Malformed { reason } => Some(reason.as_str()),
            Action::Final | Action::Command { .. } | Action::Find { .. } => None,
        });

    malformed.map_or_else(
        || {
            let attempts = highest.map_or(0, Attempt::get);

            format!("the model did not answer in {attempts} attempts")
        },
        |reason| format!("the model's answer could not be used: {reason}"),
    )
}

fn status(
    fold: &Transcript,
    ui: &Ui,
) -> String {
    match mode(fold) {
        Mode::Ended(ending) => format!("closed ({ending:?})"),
        Mode::Waiting if ui.input.len() >= MAX_SPEECH_BYTES => "the line is full".to_owned(),
        Mode::Waiting => "your turn".to_owned(),
        Mode::Working => "the agent is working".to_owned(),
    }
}

impl Ui {
    fn view(
        &self,
        fold: &Transcript,
    ) -> View {
        View {
            session: fold.session.id(),
            lines: lines(fold),
            input: self.input.clone(),
            status: status(fold, self),
            scroll: self.scroll,
        }
    }

    fn react(
        &mut self,
        key: Key,
        mode: Mode,
    ) -> Reaction {
        match key {
            Key::Char(character) => {
                if self.input.len() + character.len_utf8() <= MAX_SPEECH_BYTES {
                    self.input.push(character);
                }

                Reaction::Idle
            },
            Key::Backspace => {
                self.input.pop();

                Reaction::Idle
            },
            Key::Enter => match mode {
                Mode::Waiting if !self.input.trim().is_empty() => {
                    Reaction::Speak(mem::take(&mut self.input))
                },
                Mode::Waiting | Mode::Working | Mode::Ended(_) => Reaction::Idle,
            },
            Key::Up => {
                self.scroll = self.scroll.saturating_add(1);

                Reaction::Idle
            },
            Key::Down => {
                self.scroll = self.scroll.saturating_sub(1);

                Reaction::Idle
            },
            Key::Interrupt => Reaction::Quit,
        }
    }

    fn read_line(
        &mut self,
        screen: &mut impl Screen,
        fold: &Transcript,
    ) -> Result<Option<String>, Error> {
        loop {
            screen.draw(&self.view(fold))?;

            let key = match screen.event()? {
                Event::Resize => continue,
                Event::Key(key) => key,
            };

            match self.react(key, mode(fold)) {
                Reaction::Idle => {},
                Reaction::Speak(line) => return Ok(Some(line)),
                Reaction::Quit => return Ok(None),
            }
        }
    }
}

/// Runs the conversation until the session ends or the human leaves.
///
/// The loop does what the domain's answer names: one bounded step, or — when the fold says the
/// conversation belongs to the human — one line read and committed. Nothing here knows what a turn,
/// a budget, a closure, or a fold refusal is.
///
/// # Errors
///
/// Returns the engine's or the screen's own failure. A provider failure is not one of them: the
/// turn spends its next attempt instead.
pub async fn run(
    engine: &mut impl Engine,
    screen: &mut impl Screen,
) -> Result<Outcome, Error> {
    let mut ui = Ui::default();

    loop {
        let fold = engine.fold()?;
        screen.draw(&ui.view(&fold))?;

        match mode(&fold) {
            Mode::Waiting => match ui.read_line(screen, &fold)? {
                Some(line) => engine.speak(&line)?,
                None => {
                    return Ok(Outcome {
                        ending: None,
                        lines: ui.view(&fold).lines,
                    });
                },
            },
            Mode::Working => match engine.step().await {
                // A replayed intent is a write that landed with no outcome after it: re-read the log,
                // which settles it, and step again. A cancel from another process left work in
                // flight, and recovery on the reopen settles it before the next step closes.
                Ok(Progress::Yielded | Progress::Advanced(Move::Halt(Halt::Draining))) => {
                    engine.reopen()?;
                },
                // Why a provider failure does not end the conversation:
                // `mokmok-agent-runtime/src/lib.rs` states that an interrupted model call is
                // re-asked under a new attempt with byte-identical context, while an interrupted
                // tool call may have changed the world and is never re-dispatched. So the turn
                // spends its next attempt, and the attempt ceiling settles it failed.
                Ok(Progress::Advanced(_) | Progress::Awaiting)
                | Err(Error::Agent(mokmok_agent_runtime::Error::Provider(_))) => {},
                Ok(Progress::Terminal(ending)) => {
                    return Ok(Outcome {
                        ending: Some(ending),
                        lines: ui.view(&fold).lines,
                    });
                },
                Err(failure) => return Err(failure),
            },
            Mode::Ended(ending) => {
                return Ok(Outcome {
                    ending: Some(ending),
                    lines: ui.view(&fold).lines,
                });
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use proptest::prelude::*;

    use super::{Mode, Reaction, Ui, lines, status};
    use crate::Error;
    use crate::port::{Event, Key, Line, Screen, View};
    use mokmok_agent_runtime::{
        Action, AnswerRecord, ArtifactRef, Budget, CallRecord, MAX_SPEECH_BYTES, Record, Setting,
        SpeechRecord, Task, TaskRecord, Transcript, UNKNOWN_WARNING, Usage, execution_id, turn_id,
    };
    use mokmok_protocol::{SessionID, TurnID};
    use mokmok_state_machine::{
        Attempt, Deadline, Fact, Resolution, TurnFact, TurnNumber, TurnOutcome,
    };

    fn session() -> SessionID {
        SessionID::from_uuid(uuid::Uuid::from_u128(1))
    }

    fn artifact(byte: u8) -> ArtifactRef {
        ArtifactRef {
            hash: format!("{byte:02x}"),
            bytes: 1,
            truncated: false,
        }
    }

    fn fold_of(records: &[Record]) -> Transcript {
        Transcript::replay(session(), records).expect("a hand-written history folds")
    }

    fn answer(
        turn: TurnID,
        action: Action,
    ) -> Record {
        Record::Answer(AnswerRecord {
            turn,
            attempt: Attempt::new(1),
            action,
            prose: String::new(),
            text: artifact(0xaa),
            model: "test".to_owned(),
            usage: Usage {
                input: 0,
                output: 0,
            },
        })
    }

    fn opened(turn: TurnID) -> Vec<Record> {
        vec![
            Record::Turn {
                turn,
                fact: TurnFact::TurnOpened {
                    number: TurnNumber::new(1),
                },
            },
            Record::Speech(SpeechRecord {
                turn,
                text: "hello".to_owned(),
            }),
            Record::Turn {
                turn,
                fact: TurnFact::ModelRequested {
                    attempt: Attempt::new(1),
                },
            },
        ]
    }

    /// The task record a session commits when it begins.
    fn task() -> TaskRecord {
        TaskRecord {
            task: Task {
                goal: "hold a conversation".to_owned(),
                setting: Setting {
                    checkout: "/checkout".into(),
                    readable: Vec::new(),
                    writable: Vec::new(),
                    program: "/bin/sh".into(),
                    deadline: std::time::Duration::from_secs(120),
                    search: Vec::new(),
                },
                budget: Budget::default(),
            },
        }
    }

    /// A fold that has begun and is holding one turn the model was already asked for.
    fn in_flight(turn: TurnID) -> Vec<Record> {
        let mut records = vec![
            Record::Task(task()),
            Record::Session {
                fact: Fact::SessionOpened,
            },
        ];
        records.extend(opened(turn));

        records
    }

    /// The screen a case drives by hand: a fixed script of keys and no frame worth keeping.
    struct Scripted(VecDeque<Key>);

    impl Screen for Scripted {
        fn event(&mut self) -> Result<Event, Error> {
            self.0.pop_front().map_or_else(
                || {
                    Err(Error::Terminal(std::io::Error::other(
                        "the script is spent",
                    )))
                },
                |key| Ok(Event::Key(key)),
            )
        }

        fn draw(
            &mut self,
            _view: &View,
        ) -> Result<(), Error> {
            Ok(())
        }
    }

    #[test]
    fn a_turn_the_model_never_answered_draws_the_trouble() {
        let turn = turn_id(session(), 1);
        let mut records = opened(turn);
        records.push(Record::Turn {
            turn,
            fact: TurnFact::ModelAnswered,
        });
        records.push(answer(
            turn,
            Action::Malformed {
                reason: "the tool arguments are not json".to_owned(),
            },
        ));
        records.push(Record::Turn {
            turn,
            fact: TurnFact::TurnSettled {
                outcome: TurnOutcome::Failed,
            },
        });

        assert_eq!(
            lines(&fold_of(&records)),
            vec![
                Line::Human("hello".to_owned()),
                Line::Trouble(
                    "the model's answer could not be used: the tool arguments are not json"
                        .to_owned(),
                ),
            ],
        );
    }

    #[test]
    fn a_turn_with_a_recorded_call_draws_its_work() {
        let turn = turn_id(session(), 1);
        let execution = execution_id(turn);
        let mut records = opened(turn);
        records.push(Record::Turn {
            turn,
            fact: TurnFact::ModelAnswered,
        });
        records.push(answer(
            turn,
            Action::Command {
                command: "cargo test".to_owned(),
            },
        ));
        records.push(Record::Turn {
            turn,
            fact: TurnFact::ToolRequested {
                execution,
                deadline: Deadline::Bounded,
            },
        });
        records.push(Record::Call(CallRecord {
            turn,
            execution,
            command: "cargo test".to_owned(),
        }));
        records.push(Record::Turn {
            turn,
            fact: TurnFact::ToolResolved {
                execution,
                resolution: Resolution::Finished { code: 7 },
            },
        });
        records.push(Record::Turn {
            turn,
            fact: TurnFact::TurnSettled {
                outcome: TurnOutcome::Failed,
            },
        });

        assert_eq!(
            lines(&fold_of(&records)),
            vec![
                Line::Human("hello".to_owned()),
                Line::Work {
                    command: "cargo test".to_owned(),
                    observation: "exit code 7".to_owned(),
                },
            ],
        );
    }

    #[test]
    fn a_settled_turn_that_dispatched_a_command_without_a_call_record_draws_its_work() {
        let turn = turn_id(session(), 1);
        let execution = execution_id(turn);
        let mut records = vec![Record::Session {
            fact: Fact::SessionOpened,
        }];
        records.extend(opened(turn));
        records.push(Record::Turn {
            turn,
            fact: TurnFact::ModelAnswered,
        });
        records.push(answer(
            turn,
            Action::Command {
                command: "cargo test".to_owned(),
            },
        ));
        records.push(Record::Session {
            fact: Fact::ExecutionLaunched {
                execution,
                deadline: Deadline::Bounded,
            },
        });
        records.push(Record::Turn {
            turn,
            fact: TurnFact::ToolRequested {
                execution,
                deadline: Deadline::Bounded,
            },
        });
        records.push(Record::Turn {
            turn,
            fact: TurnFact::ToolResolved {
                execution,
                resolution: Resolution::Unknown,
            },
        });
        records.push(Record::Turn {
            turn,
            fact: TurnFact::TurnSettled {
                outcome: TurnOutcome::Failed,
            },
        });

        assert_eq!(
            lines(&fold_of(&records)),
            vec![
                Line::Human("hello".to_owned()),
                Line::Work {
                    command: "(the command was not recorded)".to_owned(),
                    observation: UNKNOWN_WARNING.to_owned(),
                },
            ],
        );
    }

    #[test]
    fn a_malformed_answer_at_an_earlier_attempt_yields_to_the_highest_attempt() {
        let turn = turn_id(session(), 1);
        let mut records = opened(turn);
        records.push(Record::Turn {
            turn,
            fact: TurnFact::ModelAnswered,
        });
        records.push(answer(
            turn,
            Action::Malformed {
                reason: "the tool arguments are not json".to_owned(),
            },
        ));
        records.push(Record::Turn {
            turn,
            fact: TurnFact::ModelRequested {
                attempt: Attempt::new(2),
            },
        });
        records.push(Record::Turn {
            turn,
            fact: TurnFact::TurnSettled {
                outcome: TurnOutcome::Failed,
            },
        });

        assert_eq!(
            lines(&fold_of(&records)),
            vec![
                Line::Human("hello".to_owned()),
                Line::Trouble("the model did not answer in 2 attempts".to_owned()),
            ],
        );
    }

    #[test]
    fn an_empty_line_is_not_sent() {
        let mut empty = Ui::default();
        assert_eq!(
            empty.react(Key::Enter, Mode::Waiting),
            Reaction::Idle,
            "an empty line must not open a turn",
        );
        assert_eq!(empty.input, "", "the input is left as the human typed it");

        let mut spaces = Ui {
            input: "   ".to_owned(),
            ..Ui::default()
        };
        assert_eq!(
            spaces.react(Key::Enter, Mode::Waiting),
            Reaction::Idle,
            "a line of spaces holds nothing to say",
        );
        assert_eq!(
            spaces.input, "   ",
            "the input is left as the human typed it"
        );

        let mut one = Ui {
            input: "x".to_owned(),
            ..Ui::default()
        };
        assert_eq!(
            one.react(Key::Enter, Mode::Waiting),
            Reaction::Speak("x".to_owned()),
            "a line with one character still holds something to say",
        );
    }

    #[test]
    fn an_unopened_session_waits_for_the_human() {
        assert_eq!(
            status(&fold_of(&[Record::Task(task())]), &Ui::default()),
            "your turn"
        );
    }

    #[test]
    fn a_session_with_a_turn_in_flight_reports_the_agent_working() {
        let fold = fold_of(&in_flight(turn_id(session(), 1)));

        assert_eq!(status(&fold, &Ui::default()), "the agent is working");
    }

    #[test]
    fn a_closed_session_reports_its_ending() {
        let records = vec![
            Record::Task(task()),
            Record::Session {
                fact: Fact::SessionOpened,
            },
            Record::Session {
                fact: Fact::SessionClosed,
            },
        ];

        assert_eq!(
            status(&fold_of(&records), &Ui::default()),
            "closed (Completed)"
        );
    }

    #[test]
    fn a_line_at_the_byte_ceiling_is_reported_as_full() {
        let fold = fold_of(&[Record::Task(task())]);
        let full = Ui {
            input: "x".repeat(MAX_SPEECH_BYTES),
            ..Ui::default()
        };
        let almost = Ui {
            input: "x".repeat(MAX_SPEECH_BYTES - 1),
            ..Ui::default()
        };

        assert_eq!(status(&fold, &full), "the line is full");
        assert_eq!(
            status(&fold, &almost),
            "your turn",
            "a line one byte short of the ceiling still holds something to say",
        );
    }

    #[test]
    fn a_typed_character_extends_the_line() {
        let mut ui = Ui::default();

        assert_eq!(ui.react(Key::Char('a'), Mode::Waiting), Reaction::Idle);
        assert_eq!(ui.input, "a");
    }

    #[test]
    fn a_character_the_line_cannot_hold_is_dropped_whole() {
        let mut full = Ui {
            input: "x".repeat(MAX_SPEECH_BYTES),
            ..Ui::default()
        };
        assert_eq!(full.react(Key::Char('a'), Mode::Waiting), Reaction::Idle);
        assert_eq!(full.input.len(), MAX_SPEECH_BYTES);

        let mut almost = Ui {
            input: "x".repeat(MAX_SPEECH_BYTES - 1),
            ..Ui::default()
        };
        assert_eq!(almost.react(Key::Char('é'), Mode::Waiting), Reaction::Idle);
        assert_eq!(
            almost.input.len(),
            MAX_SPEECH_BYTES - 1,
            "a two-byte character must not be cut to fit the last byte",
        );
    }

    #[test]
    fn a_turn_in_flight_draws_the_human_line_and_no_trouble() {
        let fold = fold_of(&in_flight(turn_id(session(), 1)));

        assert_eq!(lines(&fold), vec![Line::Human("hello".to_owned())]);
    }

    #[test]
    fn read_line_returns_the_line_a_script_typed_and_nothing_when_the_human_leaves() {
        let fold = fold_of(&[Record::Task(task())]);
        let mut ui = Ui::default();
        let mut typed = Scripted(VecDeque::from([Key::Char('a'), Key::Char('b'), Key::Enter]));

        assert_eq!(
            ui.read_line(&mut typed, &fold).expect("the script is read"),
            Some("ab".to_owned()),
        );
        assert_eq!(ui.input, "", "a submitted line leaves the input");

        let mut left = Scripted(VecDeque::from([Key::Interrupt]));

        assert_eq!(
            Ui::default()
                .read_line(&mut left, &fold)
                .expect("the script is read"),
            None,
        );
    }

    proptest! {
        #[test]
        fn typing_never_carries_the_line_past_its_ceiling(typed in prop::collection::vec(any::<char>(), 0..64)) {
            let mut ui = Ui::default();

            for character in typed {
                ui.react(Key::Char(character), Mode::Waiting);
            }

            prop_assert!(
                ui.input.len() <= MAX_SPEECH_BYTES,
                "the input {:?} holds {} bytes", ui.input, ui.input.len(),
            );
        }
    }
}
