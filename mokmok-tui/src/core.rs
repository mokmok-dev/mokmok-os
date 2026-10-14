//! The pure core: two fields of state, three pure functions, one driver.

use std::mem;

use mokmok_agent_runtime::{ContextAct, Halt, MAX_SPEECH_BYTES, Move, Progress, Transcript, next};
use mokmok_state_machine::{Ending, Phase};

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

    for turn in fold.shown() {
        if let Some(speech) = turn.speech {
            lines.push(Line::Human(speech));
        }

        match turn.act {
            Some(ContextAct::Ran {
                command,
                observation,
            }) => lines.push(Line::Work {
                command,
                observation,
            }),
            Some(ContextAct::Said { reply }) => lines.push(Line::Agent(reply)),
            None => {},
        }
    }

    lines
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
/// Returns the engine's or the screen's own failure.
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
            Mode::Working => match engine.step().await? {
                // A replayed intent is a write that landed with no outcome after it: re-read the log,
                // which settles it, and step again. A cancel from another process left work in
                // flight, and recovery on the reopen settles it before the next step closes.
                Progress::Yielded | Progress::Advanced(Move::Halt(Halt::Draining)) => {
                    engine.reopen()?;
                },
                Progress::Advanced(_) | Progress::Awaiting => {},
                Progress::Terminal(ending) => {
                    return Ok(Outcome {
                        ending: Some(ending),
                        lines: ui.view(&fold).lines,
                    });
                },
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
    use super::{Mode, Reaction, Ui};
    use crate::port::Key;

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
}
