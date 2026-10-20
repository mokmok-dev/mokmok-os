//! The pure core: the sessions one process holds, one pane, three pure functions, one driver.

use std::cell::RefCell;
use std::mem;
use std::rc::Rc;
use std::time::Duration;

use mokmok_agent_runtime::{
    Action, ContextAct, Halt, MAX_SPEECH_BYTES, Move, Progress, Transcript, next,
};
use mokmok_protocol::{SessionID, TurnID};
use mokmok_state_machine::{Attempt, Ending, Phase};
use tokio::time::{MissedTickBehavior, interval};

use crate::port::{
    Engine, Entry, Event, Key, Line, Link, Listing, Roster, Screen, Sheet, Update, View,
};
use crate::{Error, Left, Outcome};

/// One session this process holds: its engine, the line being typed in it, and how far back its
/// transcript is scrolled.
///
/// The line and the scroll belong to the session rather than to the screen, so switching away and
/// back leaves what the human was typing where he left it.
struct Held<E> {
    engine: E,
    input: String,
    scroll: u16,
    streaming: Rc<RefCell<String>>,
    /// Why the last command or record could not land, drawn until the next line is tried.
    trouble: String,
    /// Whether the bus the session reads is answering.
    link: Link,
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
    Idle,
    Speak(String),
    Quit,
    List,
    Adopt(SessionID),
    Create(String),
}

#[derive(Debug, Eq, PartialEq)]
enum Pane {
    List { cursor: usize },
    New { goal: String },
}

/// The sessions this process holds, the one the human is in, and the pane he opened.
///
/// Every question about the screen is answered here from the folds the caller hands in, so the frame
/// and the keys are testable without a log, a model, or a terminal.
struct Manager<E> {
    held: Vec<Held<E>>,
    focus: usize,
    /// The log's own listing, as it was when the listing pane was opened.
    listed: Vec<Listing>,
    pane: Option<Pane>,
    deferred: Vec<Event>,
    /// Which frame of the spinner the screen shows while the focused session is moving.
    phase: u8,
}

/// Whose turn it is. The one question the driver asks, and a pure function of the fold.
fn mode(fold: &Transcript) -> Mode {
    match fold.session.phase() {
        Phase::Closed(ending) => Mode::Ended(ending),
        // A cancel in force from another process: the loop settles what is in flight, then closes it.
        Phase::Draining => Mode::Working,
        // An unopened session is opened by its first line, not by a model call with nothing to answer.
        Phase::Unopened | Phase::Accepting => {
            if working(fold) {
                Mode::Working
            } else {
                Mode::Waiting
            }
        },
    }
}

/// Whether the session owes another step.
///
/// The bus engine uses the same rule to report itself busy, so the conversation never asks a
/// remote session to step locally and never parks while it is still working.
pub(crate) fn working(fold: &Transcript) -> bool {
    match fold.session.phase() {
        Phase::Closed(_) => false,
        Phase::Draining => true,
        Phase::Unopened | Phase::Accepting => {
            !matches!(next(fold), Move::Begin { .. } | Move::Await)
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
            Some(ContextAct::Edited { observation }) => {
                lines.push(Line::Edit { observation });
            },
            Some(ContextAct::Subagent {
                child_session,
                goal,
                reply,
            }) => {
                lines.push(Line::Delegate {
                    child_session,
                    goal,
                    reply,
                });
            },
            // A loaded skill is not one of the agent's own words, so it draws as its own line; a
            // refused load made the turn fail, so it is the reason the turn could not finish.
            Some(ContextAct::Loaded { name }) => {
                lines.push(Line::Agent(format!("loaded skill {name}")));
            },
            Some(ContextAct::Refused { name, reason }) => {
                lines.push(Line::Trouble(format!(
                    "the skill {name} could not be loaded: {reason}"
                )));
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
            Action::Final
            | Action::Command { .. }
            | Action::Find { .. }
            | Action::Edit { .. }
            | Action::UseSkill { .. }
            | Action::Subagent { .. } => None,
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
    input: &str,
) -> String {
    match mode(fold) {
        Mode::Ended(ending) => closed_phrase(ending),
        Mode::Waiting if input.len() >= MAX_SPEECH_BYTES => "the line is full".to_owned(),
        Mode::Waiting => "your turn".to_owned(),
        Mode::Working => "the agent is working".to_owned(),
    }
}

fn closed_phrase(ending: Ending) -> String {
    format!("closed ({ending:?})")
}

/// One phrase for the phase a session reached, for a session this process holds no engine for.
fn phase_phrase(phase: Phase) -> String {
    match phase {
        Phase::Unopened => "not opened".to_owned(),
        Phase::Accepting => "open".to_owned(),
        Phase::Draining => "draining".to_owned(),
        Phase::Closed(ending) => closed_phrase(ending),
    }
}

/// One phrase for a session only the log holds, saying when its task is the thing the log does not
/// hold.
///
/// The runtime continues a session under the task its log recorded and has no other goal to continue
/// one under, so a session this build reads no task from is marked where the human chooses it rather
/// than after he has chosen it.
fn listed_phrase(listing: &Listing) -> String {
    let phase = phase_phrase(listing.phase);

    match listing.goal {
        None => format!("{phase} {NO_TASK}"),
        Some(_) => phase,
    }
}

/// What a session the log holds no task for says about itself.
const NO_TASK: &str = "(no task recorded)";

fn background(
    folds: &[Transcript],
    focus: usize,
) -> Option<String> {
    let working = folds
        .iter()
        .enumerate()
        .filter(|(index, fold)| *index != focus && matches!(mode(fold), Mode::Working))
        .count();

    match working {
        0 => None,
        1 => Some("1 other session working".to_owned()),
        other => Some(format!("{other} other sessions working")),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Keep {
    Running,
    Leaving,
}

fn settled(folds: &[Transcript]) -> bool {
    folds
        .iter()
        .all(|fold| matches!(mode(fold), Mode::Ended(_)))
}

fn outcome<E>(
    held: &[Held<E>],
    folds: &[Transcript],
) -> Outcome {
    Outcome {
        left: folds
            .iter()
            .zip(held)
            .map(|(fold, held)| {
                let mut shown = lines(fold);
                if !held.trouble.is_empty() {
                    shown.push(Line::Trouble(held.trouble.clone()));
                }

                Left {
                    session: fold.session.id(),
                    ending: match mode(fold) {
                        Mode::Ended(ending) => Some(ending),
                        Mode::Waiting | Mode::Working => None,
                    },
                    lines: shown,
                }
            })
            .collect(),
    }
}

const HELD_LEGEND: &str = "* is held";

impl<E: Engine> Manager<E> {
    fn outcome(&self) -> Result<Outcome, Error> {
        let folds = self.folds()?;

        Ok(outcome(&self.held, &folds))
    }

    fn holding(engine: E) -> Self {
        Self {
            held: vec![Held {
                engine,
                input: String::new(),
                scroll: 0,
                streaming: Rc::new(RefCell::new(String::new())),
                trouble: String::new(),
                link: Link::Connected,
            }],
            focus: 0,
            listed: Vec::new(),
            pane: None,
            deferred: Vec::new(),
            phase: 0,
        }
    }

    fn take_deferred(&mut self) -> Vec<Event> {
        mem::take(&mut self.deferred)
    }

    fn flush_deferred(
        &mut self,
        folds: &[Transcript],
        world: &impl Roster<Engine = E>,
    ) -> Result<Keep, Error> {
        for event in self.take_deferred() {
            if self.event(event, folds, world)? == Keep::Leaving {
                return Ok(Keep::Leaving);
            }
        }

        Ok(Keep::Running)
    }

    /// Every held session's past, in roster order.
    ///
    /// # Errors
    ///
    /// Returns the journal's failure when one session's history cannot be read or does not fold.
    fn folds(&self) -> Result<Vec<Transcript>, Error> {
        self.held.iter().map(|held| held.engine.fold()).collect()
    }

    fn view(
        &self,
        folds: &[Transcript],
    ) -> View {
        let held = &self.held[self.focus];
        let fold = &folds[self.focus];
        let mut shown = lines(fold);
        let streaming = held.streaming.borrow();

        if !streaming.is_empty() {
            shown.push(Line::Agent(streaming.clone()));
        }
        if !held.trouble.is_empty() {
            shown.push(Line::Trouble(held.trouble.clone()));
        }

        View {
            session: fold.session.id(),
            goal: fold.task.as_ref().map(|task| task.goal.clone()),
            checkout: fold.task.as_ref().map(|task| task.setting.checkout.clone()),
            lines: shown,
            input: held.input.clone(),
            status: self.status(folds),
            hints: self.hints(folds),
            placeholder: self.placeholder(folds),
            scroll: held.scroll,
            background: background(folds, self.focus),
            sheet: self.sheet(folds),
            spinner: matches!(mode(fold), Mode::Working).then_some(self.phase),
        }
    }

    fn hints(
        &self,
        folds: &[Transcript],
    ) -> String {
        match &self.pane {
            Some(Pane::List { .. }) => "↑↓ choose  enter open  esc close".to_owned(),
            Some(Pane::New { .. }) => "enter create  esc cancel".to_owned(),
            None => match mode(&folds[self.focus]) {
                Mode::Ended(_) => "tab next  ^O sessions  ^N new  ^C quit".to_owned(),
                Mode::Waiting | Mode::Working => {
                    "enter send  tab next  ^O sessions  ^N new  ^C quit".to_owned()
                },
            },
        }
    }

    fn placeholder(
        &self,
        folds: &[Transcript],
    ) -> String {
        match &self.pane {
            Some(Pane::List { .. }) => String::new(),
            Some(Pane::New { .. }) => "what should this session do?".to_owned(),
            None => match mode(&folds[self.focus]) {
                Mode::Waiting => "Send a line to the agent…".to_owned(),
                Mode::Working | Mode::Ended(_) => String::new(),
            },
        }
    }

    /// Advances the spinner, but only while the session on screen is the one moving.
    fn advance(
        &mut self,
        folds: &[Transcript],
    ) {
        if matches!(mode(&folds[self.focus]), Mode::Working) {
            self.phase = self.phase.wrapping_add(1);
        }
    }

    fn status(
        &self,
        folds: &[Transcript],
    ) -> String {
        match &self.pane {
            Some(Pane::List { .. }) => {
                let count = self.entries(folds).len();

                match count {
                    1 => format!("1 session · {HELD_LEGEND}"),
                    _ => format!("{count} sessions · {HELD_LEGEND}"),
                }
            },
            Some(Pane::New { .. }) => "a new session".to_owned(),
            None if self.held[self.focus].link == Link::Reconnecting => {
                "the bus is not answering".to_owned()
            },
            None => status(&folds[self.focus], &self.held[self.focus].input),
        }
    }

    fn sheet(
        &self,
        folds: &[Transcript],
    ) -> Option<Sheet> {
        match &self.pane {
            None => None,
            Some(Pane::List { cursor }) => Some(Sheet::List {
                entries: self.entries(folds),
                cursor: *cursor,
            }),
            Some(Pane::New { goal }) => Some(Sheet::New { goal: goal.clone() }),
        }
    }

    fn entries(
        &self,
        folds: &[Transcript],
    ) -> Vec<Entry> {
        let mut entries: Vec<Entry> = self
            .held
            .iter()
            .enumerate()
            .map(|(index, held)| Entry {
                session: held.engine.session(),
                goal: folds[index].task.as_ref().map(|task| task.goal.clone()),
                status: status(&folds[index], &held.input),
                held: true,
                focused: index == self.focus,
            })
            .collect();

        entries.extend(
            self.listed
                .iter()
                .filter(|listing| self.index_of(listing.session).is_none())
                .map(|listing| Entry {
                    session: listing.session,
                    goal: listing.goal.clone(),
                    status: listed_phrase(listing),
                    held: false,
                    focused: false,
                }),
        );

        entries
    }

    fn index_of(
        &self,
        session: SessionID,
    ) -> Option<usize> {
        self.held
            .iter()
            .position(|held| held.engine.session() == session)
    }

    /// The held session to step next: the first one in rotation from the focus whose turn it is.
    ///
    /// The rotation starts at the focus, so the session the human is watching moves first and the
    /// ones behind it take their turns in between his keystrokes.
    fn next_to_step(
        &self,
        folds: &[Transcript],
    ) -> Option<usize> {
        let count = folds.len();

        (0..count)
            .map(|offset| (self.focus + offset) % count)
            .find(|index| {
                matches!(mode(&folds[*index]), Mode::Working) && !self.held[*index].engine.busy()
            })
    }

    fn any_busy(&self) -> bool {
        self.held.iter().any(|held| held.engine.busy())
    }

    /// A fragment belongs to the session's screen line until the step returns, so it is cleared
    /// then; the fold the worker reports carries the same prose, and keeping both would draw it
    /// twice.
    fn drain(&mut self) -> Result<(), Error> {
        for index in 0..self.held.len() {
            for update in self.held[index].engine.drain() {
                match update {
                    Update::Fragment(fragment) => {
                        self.held[index].streaming.borrow_mut().push_str(&fragment);
                    },
                    Update::Stepped(result) => {
                        self.held[index].streaming.borrow_mut().clear();
                        match result {
                            Ok(
                                Progress::Yielded | Progress::Advanced(Move::Halt(Halt::Draining)),
                            ) => {
                                self.held[index].engine.reopen()?;
                            },
                            Ok(_) | Err(Error::Agent(mokmok_agent_runtime::Error::Provider(_))) => {
                            },
                            Err(failure) => return Err(failure),
                        }
                    },
                    Update::Failed(failure) => {
                        self.held[index].streaming.borrow_mut().clear();

                        return Err(failure);
                    },
                    // A refusal or a trouble phrase is the session's own news: it draws where the
                    // session is, and the process keeps the other sessions and the keys.
                    Update::Refused(failure) => {
                        self.held[index].trouble = failure.to_string();
                    },
                    Update::Trouble(detail) => self.held[index].trouble = detail,
                    Update::Link(link) => self.held[index].link = link,
                }
            }
        }

        Ok(())
    }

    fn hold(
        &mut self,
        engine: E,
    ) {
        self.held.push(Held {
            engine,
            input: String::new(),
            scroll: 0,
            streaming: Rc::new(RefCell::new(String::new())),
            trouble: String::new(),
            link: Link::Connected,
        });
        self.focus = self.held.len() - 1;
        self.pane = None;
    }

    const fn cycle(&mut self) {
        self.focus = (self.focus + 1) % self.held.len();
    }

    fn open_list(
        &mut self,
        folds: &[Transcript],
    ) {
        let focused = self.held[self.focus].engine.session();
        let cursor = self
            .entries(folds)
            .iter()
            .position(|entry| entry.session == focused)
            .unwrap_or(0);

        self.pane = Some(Pane::List { cursor });
    }

    /// Opens a session the human chose, which is one this process holds or one only the log holds.
    ///
    /// # Errors
    ///
    /// Returns the errors of opening a session this process does not hold yet.
    fn adopt(
        &mut self,
        session: SessionID,
        world: &impl Roster<Engine = E>,
    ) -> Result<(), Error> {
        // Two engines on one session would make the second one's recovery mark the first one's work
        // in flight as unknown, so a session this process holds is focused and not re-opened.
        if let Some(index) = self.index_of(session) {
            self.focus = index;
            self.pane = None;
        } else if let Some(listing) = self
            .listed
            .iter()
            .find(|listing| listing.session == session)
            .cloned()
        {
            let engine = world.resume(&listing)?;
            self.hold(engine);
        }

        Ok(())
    }

    fn react(
        &mut self,
        key: Key,
        folds: &[Transcript],
    ) -> Reaction {
        if self.pane.is_some() {
            return self.react_pane(key, folds);
        }

        match key {
            Key::Char(character) => {
                self.line(|input| {
                    if input.len() + character.len_utf8() <= MAX_SPEECH_BYTES {
                        input.push(character);
                    }
                });

                Reaction::Idle
            },
            Key::Backspace => {
                self.line(|input| {
                    input.pop();
                });

                Reaction::Idle
            },
            // A line is spoken only in the conversation pane, while the session waits for one, and
            // only when it holds something to say.
            Key::Enter => match mode(&folds[self.focus]) {
                Mode::Waiting if !self.held[self.focus].input.trim().is_empty() => {
                    Reaction::Speak(mem::take(&mut self.held[self.focus].input))
                },
                Mode::Waiting | Mode::Working | Mode::Ended(_) => Reaction::Idle,
            },
            Key::Up => {
                let held = &mut self.held[self.focus];
                held.scroll = held.scroll.saturating_add(1);

                Reaction::Idle
            },
            Key::Down => {
                let held = &mut self.held[self.focus];
                held.scroll = held.scroll.saturating_sub(1);

                Reaction::Idle
            },
            Key::Next => {
                self.cycle();

                Reaction::Idle
            },
            Key::List => Reaction::List,
            Key::New => {
                self.pane = Some(Pane::New {
                    goal: String::new(),
                });

                Reaction::Idle
            },
            Key::Escape => Reaction::Idle,
            Key::Interrupt => Reaction::Quit,
        }
    }

    fn react_pane(
        &mut self,
        key: Key,
        folds: &[Transcript],
    ) -> Reaction {
        // Read before the pane is borrowed, because a cursor that moves needs the rows to clamp to.
        let entries = self.entries(folds);

        match key {
            Key::Interrupt => Reaction::Quit,
            Key::Escape | Key::List => {
                self.pane = None;

                Reaction::Idle
            },
            Key::Up | Key::Down => {
                if let Some(Pane::List { cursor }) = &mut self.pane {
                    *cursor = if matches!(key, Key::Up) {
                        cursor.saturating_sub(1)
                    } else {
                        cursor
                            .saturating_add(1)
                            .min(entries.len().saturating_sub(1))
                    };
                }

                Reaction::Idle
            },
            Key::Enter => match &mut self.pane {
                // A session the log holds no task for is not opened: the run would carry the command
                // line's goal into it and the runtime would close it on the model's first answer, so
                // the row says what it is and choosing it does nothing.
                Some(Pane::List { cursor }) => entries
                    .get(*cursor)
                    .filter(|row| row.goal.is_some())
                    .map_or(Reaction::Idle, |row| Reaction::Adopt(row.session)),
                Some(Pane::New { goal }) if !goal.trim().is_empty() => {
                    Reaction::Create(mem::take(goal))
                },
                Some(Pane::New { .. }) | None => Reaction::Idle,
            },
            Key::Char(character) => {
                if let Some(Pane::New { goal }) = &mut self.pane
                    && goal.len() + character.len_utf8() <= MAX_SPEECH_BYTES
                {
                    goal.push(character);
                }

                Reaction::Idle
            },
            Key::Backspace => {
                if let Some(Pane::New { goal }) = &mut self.pane {
                    goal.pop();
                }

                Reaction::Idle
            },
            Key::Next | Key::New => Reaction::Idle,
        }
    }

    /// Answers one thing the terminal reported.
    ///
    /// # Errors
    ///
    /// Returns the journal's refusal of a line, the log's failure when its sessions cannot be
    /// listed, and the errors of opening a session.
    fn event(
        &mut self,
        event: Event,
        folds: &[Transcript],
        world: &impl Roster<Engine = E>,
    ) -> Result<Keep, Error> {
        // A resize asks for the frame the loop draws next, and nothing else.
        let Event::Key(key) = event else {
            return Ok(Keep::Running);
        };

        let reaction = self.react(key, folds);

        self.apply(reaction, folds, world)
    }

    /// Carries out what one keystroke asked for, which is where the world is reached.
    ///
    /// # Errors
    ///
    /// Returns the journal's refusal of the line, the log's failure when its sessions cannot be
    /// listed, and the errors of opening a session.
    fn apply(
        &mut self,
        reaction: Reaction,
        folds: &[Transcript],
        world: &impl Roster<Engine = E>,
    ) -> Result<Keep, Error> {
        match reaction {
            Reaction::Quit => return Ok(Keep::Leaving),
            Reaction::Idle => {},
            Reaction::Speak(line) => self.held[self.focus].engine.speak(&line)?,
            Reaction::List => {
                self.listed = world.listed()?;
                self.open_list(folds);
            },
            Reaction::Adopt(session) => self.adopt(session, world)?,
            Reaction::Create(goal) => {
                let engine = world.create(&goal)?;
                self.hold(engine);
            },
        }

        Ok(Keep::Running)
    }

    fn line(
        &mut self,
        change: impl FnOnce(&mut String),
    ) {
        change(&mut self.held[self.focus].input);
    }

    /// Performs one bounded step of the session at `index`.
    ///
    /// # Errors
    ///
    /// Returns the failure of a step that is not the provider's own, and the terminal's failure
    /// when a streamed frame cannot be read or drawn.
    async fn step(
        &mut self,
        index: usize,
        screen: &mut impl Screen,
        folds: &[Transcript],
    ) -> Result<(), Error> {
        let on_screen = index == self.focus && self.pane.is_none();
        let base = on_screen.then(|| self.view(folds));
        let pending = Rc::clone(&self.held[index].streaming);
        let mut deferred = Vec::new();
        let mut failure = None;
        let mut phase = self.phase;

        let result = {
            let Held {
                engine,
                input,
                scroll,
                streaming: _,
                ..
            } = &mut self.held[index];
            let streamed = Rc::clone(&pending);
            let mut watch = move |fragment: &str| {
                streamed.borrow_mut().push_str(fragment);
            };
            let mut stepping = std::pin::pin!(engine.step_watched(&mut watch));
            let mut tick = interval(STREAM_TICK);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            let mut drawn = 0;

            loop {
                tokio::select! {
                    result = &mut stepping => break result,
                    _ = tick.tick(), if on_screen => {
                        phase = phase.wrapping_add(1);
                        let changed =
                            type_during_stream(screen, input, scroll, &mut deferred, &mut failure);
                        let text = pending.borrow().clone();

                        if (changed || text.len() > drawn)
                            && let Some(base) = &base
                        {
                            draw_stream(screen, base, input, *scroll, &text, phase, &mut failure);
                            drawn = text.len();
                        }
                    },
                }
            }
        };

        self.held[index].streaming.borrow_mut().clear();
        self.deferred.extend(deferred);
        self.phase = phase;

        match result {
            // A replayed intent is a write that landed with no outcome after it: re-read the log,
            // which settles it, and step again. A cancel from another process left work in flight,
            // and recovery on the reopen settles it before the next step closes.
            Ok(Progress::Yielded | Progress::Advanced(Move::Halt(Halt::Draining))) => {
                self.held[index].engine.reopen()?;
            },
            // A session that reaches its ending is left in the roster: the loop ends when every one
            // of them has, and until then the human can switch to it and read it.
            // `mokmok-agent-runtime/src/lib.rs` states why a provider failure does not end the
            // conversation: an interrupted model call is re-asked under a new attempt with
            // byte-identical context, while an interrupted tool call may have changed the world and
            // is never re-dispatched, so the turn spends its next attempt and the attempt ceiling
            // settles it failed.
            Ok(Progress::Advanced(_) | Progress::Awaiting | Progress::Terminal(_))
            | Err(Error::Agent(mokmok_agent_runtime::Error::Provider(_))) => {},
            Err(failure) => return Err(failure),
        }

        failure.map_or(Ok(()), Err)
    }
}

/// How often a frame is redrawn while a step runs. Sixteen milliseconds is one frame at sixty
/// hertz.
const STREAM_TICK: Duration = Duration::from_millis(16);

fn type_during_stream(
    screen: &mut impl Screen,
    input: &mut String,
    scroll: &mut u16,
    deferred: &mut Vec<Event>,
    failure: &mut Option<Error>,
) -> bool {
    let mut changed = false;

    loop {
        match screen.poll() {
            Ok(Some(Event::Key(Key::Char(character)))) => {
                if input.len() + character.len_utf8() <= MAX_SPEECH_BYTES {
                    input.push(character);
                    changed = true;
                }
            },
            Ok(Some(Event::Key(Key::Backspace))) => {
                input.pop();
                changed = true;
            },
            Ok(Some(Event::Key(Key::Up))) => {
                *scroll = scroll.saturating_add(1);
                changed = true;
            },
            Ok(Some(Event::Key(Key::Down))) => {
                *scroll = scroll.saturating_sub(1);
                changed = true;
            },
            Ok(Some(Event::Resize)) => changed = true,
            Ok(Some(event)) => deferred.push(event),
            Ok(None) => break,
            Err(error) => {
                failure.get_or_insert(error);
                break;
            },
        }
    }

    changed
}

fn draw_stream(
    screen: &mut impl Screen,
    base: &View,
    input: &str,
    scroll: u16,
    pending: &str,
    phase: u8,
    failure: &mut Option<Error>,
) {
    let mut view = base.clone();
    view.input.clear();
    view.input.push_str(input);
    view.scroll = scroll;
    view.lines.push(Line::Agent(pending.to_owned()));
    view.status.clear();
    view.status.push_str("the agent is answering");
    view.spinner = Some(phase);

    if let Err(error) = screen.draw(&view) {
        failure.get_or_insert(error);
    }
}

/// Runs every session the process holds until they have all ended or the human leaves.
///
/// The loop does what the domain's answer names: one bounded step of the session whose turn it is,
/// or — when the human is the one who owes something — one line read and committed. Nothing here
/// knows what a turn, a budget, a closure, or a fold refusal is.
///
/// # Errors
///
/// Returns the engine's or the screen's own failure. A provider failure is not one of them: the
/// turn spends its next attempt instead.
pub async fn run<R: Roster>(
    world: &R,
    first: R::Engine,
    screen: &mut impl Screen,
) -> Result<Outcome, Error> {
    let mut manager = Manager::holding(first);

    loop {
        // A driver whose step answered without yielding would spin against the executor, and a
        // caller's deadline could never fire. One yield per pass keeps the loop cancellable.
        tokio::task::yield_now().await;
        manager.drain()?;

        let mut folds = manager.folds()?;

        if manager.flush_deferred(&folds, world)? == Keep::Leaving {
            return manager.outcome();
        }

        folds = manager.folds()?;

        screen.draw(&manager.view(&folds))?;
        manager.advance(&folds);

        if settled(&folds) {
            return manager.outcome();
        }

        if let Some(index) = manager.next_to_step(&folds) {
            // What the human pressed is answered before another session takes its step, so typing
            // never has to wait for a session he is not looking at.
            match screen.poll()? {
                Some(event) => {
                    if manager.event(event, &folds, world)? == Keep::Leaving {
                        return manager.outcome();
                    }
                },
                None => manager.step(index, screen, &folds).await?,
            }
        } else if manager.any_busy() {
            // The screen keeps reading keys and drawing the fragments they report, then waits one
            // frame so a silent step does not spin the loop.
            match screen.poll()? {
                Some(event) => {
                    if manager.event(event, &folds, world)? == Keep::Leaving {
                        return manager.outcome();
                    }
                },
                None => tokio::time::sleep(STREAM_TICK).await,
            }
        } else {
            // Nothing can move, so the process waits on the human and on nothing else.
            if manager.event(screen.event()?, &folds, world)? == Keep::Leaving {
                return manager.outcome();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::Duration;

    use proptest::prelude::*;

    use super::{
        Keep, Manager, Mode, Pane, Reaction, background, lines, mode, phase_phrase, run, settled,
        status,
    };
    use crate::Error;
    use crate::port::{Engine, Entry, Event, Key, Line, Listing, Roster, Screen, Sheet, View};
    use mokmok_agent_runtime::{
        Action, AnswerRecord, ArtifactRef, Budget, CallRecord, MAX_SPEECH_BYTES, Progress, Record,
        Setting, SpeechRecord, Task, TaskRecord, Transcript, UNKNOWN_WARNING, Usage, execution_id,
        turn_id,
    };
    use mokmok_protocol::{SessionID, TurnID};
    use mokmok_state_machine::{
        Attempt, Deadline, Ending, Fact, Phase, Resolution, TurnFact, TurnNumber, TurnOutcome,
    };

    fn session() -> SessionID {
        SessionID::from_uuid(uuid::Uuid::from_u128(1))
    }

    fn others(seed: u128) -> SessionID {
        SessionID::from_uuid(uuid::Uuid::from_u128(seed))
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
        task_of("hold a conversation")
    }

    fn task_of(goal: &str) -> TaskRecord {
        TaskRecord {
            task: Task {
                goal: goal.to_owned(),
                setting: Setting {
                    checkout: "/checkout".into(),
                    readable: Vec::new(),
                    writable: Vec::new(),
                    program: "/bin/sh".into(),
                    deadline: std::time::Duration::from_secs(120),
                    search: Vec::new(),
                    skills: Vec::new(),
                    delegate: false,
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

    fn idle() -> Vec<Record> {
        vec![
            Record::Task(task()),
            Record::Session {
                fact: Fact::SessionOpened,
            },
        ]
    }

    fn awaiting() -> Vec<Record> {
        let turn = turn_id(session(), 1);
        let mut records = vec![
            Record::Task(task()),
            Record::Session {
                fact: Fact::SessionOpened,
            },
        ];
        records.extend(opened(turn));
        records.push(Record::Turn {
            turn,
            fact: TurnFact::ModelAnswered,
        });
        records.push(answer(turn, Action::Final));
        records.push(Record::Turn {
            turn,
            fact: TurnFact::TurnSettled {
                outcome: TurnOutcome::Completed,
            },
        });

        records
    }

    fn closed() -> Vec<Record> {
        let mut records = idle();
        records.push(Record::Session {
            fact: Fact::SessionClosed,
        });

        records
    }

    /// An engine over a fold a case wrote, so the keys, the scheduler, and the listing are tested
    /// without a log, a model, or a terminal.
    struct Fake {
        session: SessionID,
        fold: Transcript,
    }

    impl Fake {
        fn new(
            session: SessionID,
            records: &[Record],
        ) -> Self {
            Self {
                session,
                fold: Transcript::replay(session, records).expect("a hand-written history folds"),
            }
        }

        fn awaiting() -> Self {
            Self::new(session(), &awaiting())
        }
    }

    impl Engine for Fake {
        fn session(&self) -> SessionID {
            self.session
        }

        fn fold(&self) -> Result<Transcript, Error> {
            Ok(self.fold.clone())
        }

        fn speak(
            &mut self,
            _line: &str,
        ) -> Result<(), Error> {
            Ok(())
        }

        fn step(&mut self) -> impl std::future::Future<Output = Result<Progress, Error>> {
            std::future::ready(Ok(Progress::Awaiting))
        }

        fn reopen(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

    /// A world whose sessions a case names, so the listing and the two ways of opening a session are
    /// tested without a log.
    struct FakeWorld {
        listed: Vec<Listing>,
    }

    impl FakeWorld {
        fn with(listed: Vec<Listing>) -> Self {
            Self { listed }
        }
    }

    impl Roster for FakeWorld {
        type Engine = Fake;

        fn listed(&self) -> Result<Vec<Listing>, Error> {
            Ok(self.listed.clone())
        }

        fn resume(
            &self,
            listing: &Listing,
        ) -> Result<Fake, Error> {
            Ok(Fake::new(listing.session, &awaiting()))
        }

        fn create(
            &self,
            goal: &str,
        ) -> Result<Fake, Error> {
            Ok(Fake::new(
                others(9),
                &[
                    Record::Task(task_of(goal)),
                    Record::Session {
                        fact: Fact::SessionOpened,
                    },
                ],
            ))
        }
    }

    fn listing(
        session: SessionID,
        phase: Phase,
        goal: Option<&str>,
    ) -> Listing {
        Listing {
            session,
            phase,
            goal: goal.map(str::to_owned),
            checkout: None,
        }
    }

    /// A manager holding `engines`, with the human in the last one.
    fn holding<E: Engine>(engines: Vec<E>) -> Manager<E> {
        let mut engines = engines.into_iter();
        let mut manager = Manager::holding(engines.next().expect("one session to start in"));
        for engine in engines {
            manager.hold(engine);
        }

        manager
    }

    fn folds_of(manager: &Manager<Fake>) -> Vec<Transcript> {
        manager.folds().expect("the folds read")
    }

    struct Talking {
        session: SessionID,
        working: Transcript,
        awaiting: Transcript,
        fragments: Vec<&'static str>,
        spoken: Rc<RefCell<Vec<String>>>,
        stepped: bool,
        steps: usize,
    }

    impl Talking {
        fn of(fragments: &[&'static str]) -> Self {
            Self::with_spoken(fragments, Rc::new(RefCell::new(Vec::new())))
        }

        fn with_spoken(
            fragments: &[&'static str],
            spoken: Rc<RefCell<Vec<String>>>,
        ) -> Self {
            let turn = turn_id(session(), 1);
            let mut working = vec![
                Record::Task(task()),
                Record::Session {
                    fact: Fact::SessionOpened,
                },
            ];
            working.extend(opened(turn));

            Self {
                session: session(),
                working: Transcript::replay(session(), &working)
                    .expect("a hand-written history folds"),
                awaiting: Transcript::replay(session(), &awaiting())
                    .expect("a hand-written history folds"),
                fragments: fragments.to_vec(),
                spoken,
                stepped: false,
                steps: 0,
            }
        }
    }

    impl Engine for Talking {
        fn session(&self) -> SessionID {
            self.session
        }

        fn fold(&self) -> Result<Transcript, Error> {
            Ok(if self.stepped {
                self.awaiting.clone()
            } else {
                self.working.clone()
            })
        }

        fn speak(
            &mut self,
            line: &str,
        ) -> Result<(), Error> {
            self.spoken.borrow_mut().push(line.to_owned());

            Ok(())
        }

        fn step(&mut self) -> impl std::future::Future<Output = Result<Progress, Error>> {
            std::future::ready(Ok(Progress::Awaiting))
        }

        async fn step_watched<'a>(
            &'a mut self,
            watch: &'a mut dyn FnMut(&str),
        ) -> Result<Progress, Error> {
            self.stepped = true;
            self.steps += 1;
            assert!(
                self.steps <= 1,
                "the loop stepped a session that was waiting",
            );

            for fragment in &self.fragments {
                watch(fragment);
                // The frame tick is what draws a fragment, so a case has to let the loop tick.
                tokio::time::sleep(Duration::from_millis(40)).await;
            }

            Ok(Progress::Awaiting)
        }

        fn reopen(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

    struct EmptyWorld {
        listed: Rc<std::cell::Cell<bool>>,
    }

    impl Roster for EmptyWorld {
        type Engine = Talking;

        fn listed(&self) -> Result<Vec<Listing>, Error> {
            self.listed.set(true);

            Ok(Vec::new())
        }

        fn resume(
            &self,
            _listing: &Listing,
        ) -> Result<Talking, Error> {
            Err(Error::Terminal(std::io::Error::other(
                "the case opens nothing",
            )))
        }

        fn create(
            &self,
            _goal: &str,
        ) -> Result<Talking, Error> {
            Err(Error::Terminal(std::io::Error::other(
                "the case creates nothing",
            )))
        }
    }

    struct Recording {
        script: VecDeque<Event>,
        after: VecDeque<Event>,
        quiet: usize,
        patience: usize,
        frames: Vec<View>,
        fail_poll: bool,
        fail_draw: bool,
    }

    impl Recording {
        fn with(events: impl IntoIterator<Item = Event>) -> Self {
            Self {
                script: events.into_iter().collect(),
                after: VecDeque::new(),
                quiet: 0,
                patience: 64,
                frames: Vec::new(),
                fail_poll: false,
                fail_draw: false,
            }
        }

        fn failing_poll() -> Self {
            Self {
                fail_poll: true,
                ..Self::with([])
            }
        }

        fn failing_draw() -> Self {
            Self {
                fail_draw: true,
                ..Self::with([])
            }
        }
    }

    impl Screen for Recording {
        fn poll(&mut self) -> Result<Option<Event>, Error> {
            assert!(
                self.patience > 0,
                "the loop kept polling after the script was spent",
            );
            self.patience -= 1;

            if self.quiet > 0 {
                self.quiet -= 1;

                return Ok(None);
            }

            if self.fail_poll {
                return Err(Error::Terminal(std::io::Error::other("the poll fails")));
            }

            Ok(self.script.pop_front())
        }

        fn event(&mut self) -> Result<Event, Error> {
            self.after
                .pop_front()
                .or_else(|| self.script.pop_front())
                .ok_or_else(|| Error::Terminal(std::io::Error::other("the script is spent")))
        }

        fn draw(
            &mut self,
            view: &View,
        ) -> Result<(), Error> {
            if self.fail_draw {
                return Err(Error::Terminal(std::io::Error::other("the draw fails")));
            }

            self.frames.push(view.clone());

            Ok(())
        }
    }

    #[tokio::test]
    async fn an_interrupt_answered_while_a_session_can_move_leaves() {
        let world = EmptyWorld {
            listed: Rc::new(std::cell::Cell::new(false)),
        };
        let first = Talking::of(&[]);
        let mut screen = Recording::with([Event::Key(Key::Interrupt)]);

        let outcome = tokio::time::timeout(Duration::from_secs(5), run(&world, first, &mut screen))
            .await
            .expect("the run ends")
            .expect("the conversation ends");

        assert_eq!(outcome.left.len(), 1);
    }

    #[tokio::test]
    async fn a_streamed_answer_is_drawn_as_it_arrives_and_typing_still_lands() {
        let mut manager = Manager::holding(Talking::of(&["one", "two"]));
        let folds = manager.folds().expect("the folds read");
        let mut screen = Recording::with([
            Event::Key(Key::Char('z')),
            Event::Key(Key::Backspace),
            Event::Key(Key::Char('q')),
            Event::Key(Key::Up),
            Event::Key(Key::Up),
            Event::Key(Key::Down),
            Event::Key(Key::Enter),
            Event::Key(Key::Next),
            Event::Resize,
        ]);

        manager
            .step(0, &mut screen, &folds)
            .await
            .expect("the step runs");

        assert!(
            screen
                .frames
                .iter()
                .any(|view| view.lines.contains(&Line::Agent("one".to_owned()))),
            "the first fragment is drawn before the second arrives",
        );
        assert!(
            screen
                .frames
                .iter()
                .any(|view| view.lines.contains(&Line::Agent("onetwo".to_owned()))),
            "the frame follows the fragments as they accumulate",
        );
        assert_eq!(
            screen.frames.last().expect("a frame").input,
            "q",
            "a character, a backspace, and another character are drawn as they are typed",
        );
        assert_eq!(manager.held[0].input, "q");
        assert_eq!(manager.held[0].scroll, 1, "up and down move the scroll");
        assert_eq!(
            manager.take_deferred(),
            vec![Event::Key(Key::Enter), Event::Key(Key::Next)],
            "a line and a session switch are answered once the step returns",
        );
    }

    #[tokio::test]
    async fn a_session_behind_the_screen_streams_without_drawing_or_reading_keys() {
        let mut manager = holding(vec![Talking::of(&["one"]), Talking::of(&["two"])]);
        let folds = manager.folds().expect("the folds read");
        let mut screen = Recording::failing_poll();

        manager
            .step(0, &mut screen, &folds)
            .await
            .expect("a background step asks the terminal for nothing");

        assert!(screen.frames.is_empty(), "no frame shows another session");
        assert_eq!(manager.take_deferred(), Vec::new());
    }

    #[tokio::test]
    async fn a_full_line_ignores_a_character_typed_while_streaming() {
        let mut manager = Manager::holding(Talking::of(&["one"]));
        manager.held[0].input = "x".repeat(MAX_SPEECH_BYTES);
        let folds = manager.folds().expect("the folds read");
        let mut screen = Recording::with([Event::Key(Key::Char('z'))]);

        manager
            .step(0, &mut screen, &folds)
            .await
            .expect("the step runs");

        assert_eq!(manager.held[0].input.len(), MAX_SPEECH_BYTES);
    }

    #[tokio::test]
    async fn a_terminal_that_fails_while_reading_ends_the_step() {
        let mut manager = Manager::holding(Talking::of(&["one"]));
        let folds = manager.folds().expect("the folds read");
        let mut screen = Recording::failing_poll();

        let error = manager
            .step(0, &mut screen, &folds)
            .await
            .expect_err("the poll fails");

        assert!(matches!(error, Error::Terminal(_)), "{error:?}");
    }

    #[tokio::test]
    async fn a_terminal_that_fails_while_drawing_ends_the_step() {
        let mut manager = Manager::holding(Talking::of(&["one"]));
        let folds = manager.folds().expect("the folds read");
        let mut screen = Recording::failing_draw();

        let error = manager
            .step(0, &mut screen, &folds)
            .await
            .expect_err("the draw fails");

        assert!(matches!(error, Error::Terminal(_)), "{error:?}");
    }

    #[tokio::test]
    async fn a_stream_draws_once_per_fragment() {
        let mut manager = Manager::holding(Talking::of(&["one"]));
        let folds = manager.folds().expect("the folds read");
        let mut screen = Recording::with([]);

        manager
            .step(0, &mut screen, &folds)
            .await
            .expect("the step runs");

        assert_eq!(screen.frames.len(), 1);
    }

    #[tokio::test]
    async fn a_pane_covering_the_session_keeps_the_stream_off_the_screen() {
        let mut manager = Manager::holding(Talking::of(&["one"]));
        manager.pane = Some(Pane::List { cursor: 0 });
        let folds = manager.folds().expect("the folds read");
        let mut screen = Recording::failing_poll();

        manager
            .step(0, &mut screen, &folds)
            .await
            .expect("a covered session steps without reading the terminal");

        assert!(screen.frames.is_empty(), "the pane stays what is on screen");
    }

    #[tokio::test]
    async fn deferred_keys_are_answered_in_order_once_the_step_returns() {
        let listed = Rc::new(std::cell::Cell::new(false));
        let world = EmptyWorld {
            listed: Rc::clone(&listed),
        };
        let mut screen = Recording::with([
            Event::Key(Key::Char('h')),
            Event::Key(Key::Char('i')),
            Event::Key(Key::Next),
            Event::Key(Key::List),
        ]);
        screen.after = [Event::Key(Key::Interrupt)].into();
        // One silence before the step, so both keys after it are typed while it runs.
        screen.quiet = 1;

        let outcome = run(&world, Talking::of(&["one"]), &mut screen)
            .await
            .expect("the conversation ends when the human leaves");

        assert!(
            listed.get(),
            "the deferred listing is answered after the step",
        );
        assert!(
            screen
                .frames
                .iter()
                .any(|view| matches!(view.sheet, Some(Sheet::List { .. }))),
            "the second deferred key is answered too",
        );
        assert_eq!(outcome.left.len(), 1);
    }

    fn react(
        manager: &mut Manager<Fake>,
        key: Key,
    ) -> Reaction {
        let folds = folds_of(manager);

        manager.react(key, &folds)
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
        let mut empty = holding(vec![Fake::awaiting()]);
        assert_eq!(
            react(&mut empty, Key::Enter),
            Reaction::Idle,
            "an empty line must not open a turn",
        );
        assert_eq!(
            empty.held[0].input, "",
            "the input is left as the human typed it",
        );

        let mut spaces = holding(vec![Fake::awaiting()]);
        spaces.held[0].input = "   ".to_owned();
        assert_eq!(
            react(&mut spaces, Key::Enter),
            Reaction::Idle,
            "a line of spaces holds nothing to say",
        );
        assert_eq!(
            spaces.held[0].input, "   ",
            "the input is left as the human typed it",
        );

        let mut one = holding(vec![Fake::awaiting()]);
        one.held[0].input = "x".to_owned();
        assert_eq!(
            react(&mut one, Key::Enter),
            Reaction::Speak("x".to_owned()),
            "a line with one character still holds something to say",
        );
        assert_eq!(
            one.held[0].input, "",
            "the line the human submitted leaves the input",
        );
    }

    #[test]
    fn a_line_waits_for_the_session_to_be_the_human_s_turn() {
        let turn = turn_id(session(), 1);
        let mut busy = holding(vec![Fake::new(session(), &in_flight(turn))]);
        busy.held[0].input = "hello".to_owned();

        assert_eq!(
            react(&mut busy, Key::Enter),
            Reaction::Idle,
            "a session whose turn the model holds takes no line",
        );

        let mut done = holding(vec![Fake::new(session(), &closed())]);
        done.held[0].input = "hello".to_owned();

        assert_eq!(
            react(&mut done, Key::Enter),
            Reaction::Idle,
            "a session that ended takes no line",
        );
    }

    #[test]
    fn an_unopened_session_waits_for_the_human() {
        assert_eq!(status(&fold_of(&[Record::Task(task())]), ""), "your turn");
    }

    #[test]
    fn a_session_with_a_turn_in_flight_reports_the_agent_working() {
        let fold = fold_of(&in_flight(turn_id(session(), 1)));

        assert_eq!(status(&fold, ""), "the agent is working");
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

        assert_eq!(status(&fold_of(&records), ""), "closed (Completed)");
    }

    #[test]
    fn a_line_at_the_byte_ceiling_is_reported_as_full() {
        let fold = fold_of(&[Record::Task(task())]);
        let full = "x".repeat(MAX_SPEECH_BYTES);
        let almost = "x".repeat(MAX_SPEECH_BYTES - 1);

        assert_eq!(status(&fold, &full), "the line is full");
        assert_eq!(
            status(&fold, &almost),
            "your turn",
            "a line one byte short of the ceiling still holds something to say",
        );
    }

    #[test]
    fn a_typed_character_extends_the_line() {
        let mut manager = holding(vec![Fake::awaiting()]);

        assert_eq!(react(&mut manager, Key::Char('a')), Reaction::Idle);
        assert_eq!(manager.held[0].input, "a");

        assert_eq!(react(&mut manager, Key::Backspace), Reaction::Idle);
        assert_eq!(
            manager.held[0].input, "",
            "a backspace takes the character back"
        );
    }

    #[test]
    fn a_character_the_line_cannot_hold_is_dropped_whole() {
        let mut full = holding(vec![Fake::awaiting()]);
        full.held[0].input = "x".repeat(MAX_SPEECH_BYTES);
        assert_eq!(react(&mut full, Key::Char('a')), Reaction::Idle);
        assert_eq!(full.held[0].input.len(), MAX_SPEECH_BYTES);

        let mut almost = holding(vec![Fake::awaiting()]);
        almost.held[0].input = "x".repeat(MAX_SPEECH_BYTES - 1);
        assert_eq!(react(&mut almost, Key::Char('é')), Reaction::Idle);
        assert_eq!(
            almost.held[0].input.len(),
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
    fn the_transcript_scrolls_one_row_at_a_time_and_stops_at_its_top() {
        let mut manager = holding(vec![Fake::awaiting()]);

        assert_eq!(react(&mut manager, Key::Down), Reaction::Idle);
        assert_eq!(
            manager.held[0].scroll, 0,
            "there is nothing above the newest row"
        );
        assert_eq!(react(&mut manager, Key::Up), Reaction::Idle);
        assert_eq!(react(&mut manager, Key::Up), Reaction::Idle);
        assert_eq!(manager.held[0].scroll, 2);
        assert_eq!(react(&mut manager, Key::Down), Reaction::Idle);
        assert_eq!(manager.held[0].scroll, 1);
    }

    #[test]
    fn the_rotation_steps_the_session_on_screen_and_then_the_ones_behind_it() {
        let turn = turn_id(session(), 1);
        let busy = || Fake::new(session(), &in_flight(turn));
        let other = || Fake::new(others(2), &in_flight(turn_id(others(2), 1)));
        let waiting = || Fake::new(others(3), &awaiting());

        let focused_busy = holding(vec![busy(), waiting()]);
        assert_eq!(
            focused_busy.next_to_step(&folds_of(&focused_busy)),
            Some(0),
            "the session the human is in moves first",
        );

        let focused_waiting = holding(vec![busy(), waiting()]);
        let mut focused_waiting = focused_waiting;
        focused_waiting.focus = 1;
        assert_eq!(
            focused_waiting.next_to_step(&folds_of(&focused_waiting)),
            Some(0),
            "a session behind the one on screen still takes its step",
        );

        let two_busy = holding(vec![busy(), other()]);
        assert_eq!(
            two_busy.next_to_step(&folds_of(&two_busy)),
            Some(1),
            "the rotation starts at the session on screen",
        );

        // Three sessions, with the one that can move furthest around the ring: stepping forwards from
        // the focus reaches it, and stepping backwards from the focus has to pass the focus.
        let mut three = holding(vec![waiting(), waiting(), waiting()]);
        three.focus = 1;
        three.held[2].engine = Fake::new(others(4), &in_flight(turn_id(others(4), 1)));
        assert_eq!(
            three.next_to_step(&folds_of(&three)),
            Some(2),
            "the rotation walks forward from the session on screen",
        );

        let quiet = holding(vec![waiting(), waiting()]);
        assert_eq!(
            quiet.next_to_step(&folds_of(&quiet)),
            None,
            "nothing moves while every session owes the human something",
        );
    }

    #[test]
    fn the_process_is_done_when_every_session_has_ended() {
        let mut one_closed = holding(vec![Fake::new(session(), &closed()), Fake::awaiting()]);
        assert!(!settled(&folds_of(&one_closed)));

        one_closed.held[1].engine = Fake::new(others(2), &closed());
        assert!(settled(&folds_of(&one_closed)));

        let none = holding(vec![Fake::new(session(), &closed())]);
        assert!(settled(&folds_of(&none)));
    }

    #[test]
    fn the_outcome_carries_every_session_it_held_and_how_each_one_stands() {
        let turn = turn_id(session(), 1);
        let manager = holding(vec![
            Fake::new(session(), &in_flight(turn)),
            Fake::new(others(2), &closed()),
        ]);

        let outcome = manager.outcome().unwrap();

        assert_eq!(outcome.left.len(), 2);
        assert_eq!(outcome.left[0].session, session());
        assert_eq!(outcome.left[0].ending, None);
        assert_eq!(outcome.left[0].lines, vec![Line::Human("hello".to_owned())],);
        assert_eq!(outcome.left[1].session, others(2));
        assert_eq!(outcome.left[1].ending, Some(Ending::Completed));
        assert_eq!(outcome.left[1].lines, Vec::new());
    }

    #[test]
    fn tab_shows_the_next_session_and_wraps_at_the_end() {
        let mut manager = holding(vec![
            Fake::new(session(), &awaiting()),
            Fake::new(others(2), &awaiting()),
        ]);
        assert_eq!(
            manager.focus, 1,
            "the last session opened is the one on screen"
        );

        assert_eq!(react(&mut manager, Key::Next), Reaction::Idle);
        assert_eq!(manager.focus, 0, "the next session wraps past the end");

        assert_eq!(react(&mut manager, Key::Next), Reaction::Idle);
        assert_eq!(manager.focus, 1);
    }

    #[test]
    fn the_other_sessions_that_are_working_are_reported_in_the_frame() {
        let turn = turn_id(session(), 1);
        let busy = |seed| Fake::new(others(seed), &in_flight(turn_id(others(seed), 1)));
        let quiet = || Fake::new(others(9), &awaiting());

        let none = vec![fold_of(&awaiting()), fold_of(&awaiting())];
        assert_eq!(background(&none, 0), None);

        let one = vec![fold_of(&awaiting()), fold_of(&in_flight(turn))];
        assert_eq!(
            background(&one, 0),
            Some("1 other session working".to_owned()),
        );
        assert_eq!(
            background(&one, 1),
            None,
            "the session on screen is not counted as the one working elsewhere",
        );

        let two = vec![fold_of(&in_flight(turn)), fold_of(&in_flight(turn))];
        assert_eq!(
            background(&two, 0),
            Some("1 other session working".to_owned()),
        );

        let manager = holding(vec![busy(2), busy(3), quiet()]);
        assert_eq!(
            manager.focus, 2,
            "the last session opened is the one on screen"
        );

        assert_eq!(
            manager.view(&folds_of(&manager)).background,
            Some("2 other sessions working".to_owned()),
        );
    }

    #[test]
    fn a_row_for_a_session_this_process_holds_carries_its_own_status_phrase() {
        let mut manager = holding(vec![
            Fake::new(session(), &in_flight(turn_id(session(), 1))),
            Fake::awaiting(),
        ]);
        manager.focus = 1;

        let rows = manager.entries(&folds_of(&manager));

        assert_eq!(
            rows,
            vec![
                Entry {
                    session: session(),
                    goal: Some("hold a conversation".to_owned()),
                    status: "the agent is working".to_owned(),
                    held: true,
                    focused: false,
                },
                Entry {
                    session: session(),
                    goal: Some("hold a conversation".to_owned()),
                    status: "your turn".to_owned(),
                    held: true,
                    focused: true,
                },
            ],
        );
    }

    #[test]
    fn every_row_says_whether_this_process_holds_the_session_and_which_one_is_on_screen() {
        let mut manager = holding(vec![Fake::awaiting()]);
        manager.listed = vec![
            listing(others(2), Phase::Unopened, None),
            listing(others(3), Phase::Accepting, Some("count the quokkas")),
            // A session this process holds is not repeated as one only the log holds.
            listing(session(), Phase::Accepting, Some("hold a conversation")),
            listing(
                others(4),
                Phase::Closed(Ending::Cancelled),
                Some("the older one"),
            ),
        ];

        let rows = manager.entries(&folds_of(&manager));

        assert_eq!(
            rows,
            vec![
                Entry {
                    session: session(),
                    goal: Some("hold a conversation".to_owned()),
                    status: "your turn".to_owned(),
                    held: true,
                    focused: true,
                },
                Entry {
                    session: others(2),
                    goal: None,
                    status: "not opened (no task recorded)".to_owned(),
                    held: false,
                    focused: false,
                },
                Entry {
                    session: others(3),
                    goal: Some("count the quokkas".to_owned()),
                    status: "open".to_owned(),
                    held: false,
                    focused: false,
                },
                Entry {
                    session: others(4),
                    goal: Some("the older one".to_owned()),
                    status: "closed (Cancelled)".to_owned(),
                    held: false,
                    focused: false,
                },
            ],
        );
    }

    #[test]
    fn a_session_that_is_draining_says_so_in_its_row() {
        assert_eq!(phase_phrase(Phase::Unopened), "not opened");
        assert_eq!(phase_phrase(Phase::Accepting), "open");
        assert_eq!(phase_phrase(Phase::Draining), "draining");
        assert_eq!(
            phase_phrase(Phase::Closed(Ending::Completed)),
            "closed (Completed)",
        );
    }

    #[test]
    fn the_listing_opens_on_the_session_on_screen() {
        let mut manager = holding(vec![Fake::awaiting(), Fake::new(others(2), &awaiting())]);
        manager.listed = vec![listing(others(3), Phase::Accepting, None)];

        assert_eq!(react(&mut manager, Key::List), Reaction::List);

        let world = FakeWorld::with(manager.listed.clone());
        let folds = folds_of(&manager);
        manager
            .apply(Reaction::List, &folds, &world)
            .expect("the listing opens");

        assert_eq!(
            manager.pane,
            Some(Pane::List { cursor: 1 }),
            "the cursor starts on the session the human is in",
        );
    }

    #[test]
    fn up_and_down_move_the_listing_cursor_and_clamp_at_both_ends() {
        let mut manager = holding(vec![Fake::awaiting()]);
        manager.listed = vec![
            listing(others(2), Phase::Accepting, None),
            listing(others(3), Phase::Accepting, None),
        ];
        manager.pane = Some(Pane::List { cursor: 0 });

        assert_eq!(react(&mut manager, Key::Up), Reaction::Idle);
        assert_eq!(
            manager.pane,
            Some(Pane::List { cursor: 0 }),
            "the cursor stops at the first row",
        );

        for _ in 0..4 {
            assert_eq!(react(&mut manager, Key::Down), Reaction::Idle);
        }
        assert_eq!(
            manager.pane,
            Some(Pane::List { cursor: 2 }),
            "the cursor stops at the last row",
        );
    }

    #[test]
    fn enter_in_the_listing_chooses_the_row_the_cursor_is_on() {
        let mut manager = holding(vec![Fake::awaiting()]);
        manager.listed = vec![listing(
            others(2),
            Phase::Accepting,
            Some("count the quokkas"),
        )];
        manager.pane = Some(Pane::List { cursor: 1 });

        assert_eq!(
            react(&mut manager, Key::Enter),
            Reaction::Adopt(others(2)),
            "the second row is the log's own session",
        );
    }

    #[test]
    fn a_session_the_log_holds_no_task_for_is_not_opened() {
        let mut manager = holding(vec![Fake::awaiting()]);
        manager.listed = vec![listing(others(2), Phase::Accepting, None)];
        manager.pane = Some(Pane::List { cursor: 1 });

        assert_eq!(
            react(&mut manager, Key::Enter),
            Reaction::Idle,
            "opening it would carry the command line's goal into a session that recorded none",
        );
        assert_eq!(
            manager.entries(&folds_of(&manager))[1].status,
            "open (no task recorded)",
            "and the row says why before the human chooses it",
        );
    }

    #[test]
    fn a_session_only_the_log_holds_is_opened_and_the_human_is_in_it() {
        let mut manager = holding(vec![Fake::awaiting()]);
        manager.listed = vec![listing(
            others(2),
            Phase::Accepting,
            Some("count the quokkas"),
        )];
        manager.pane = Some(Pane::List { cursor: 1 });

        let world = FakeWorld::with(manager.listed.clone());
        let folds = folds_of(&manager);
        manager
            .apply(Reaction::Adopt(others(2)), &folds, &world)
            .expect("the session opens");

        assert_eq!(manager.held.len(), 2);
        assert_eq!(manager.focus, 1);
        assert_eq!(manager.held[1].engine.session(), others(2));
        assert_eq!(manager.pane, None, "choosing a session closes the listing");
    }

    #[test]
    fn a_session_this_process_holds_is_focused_instead_of_opened_a_second_time() {
        let mut manager = holding(vec![
            Fake::new(session(), &awaiting()),
            Fake::new(others(2), &awaiting()),
        ]);
        manager.pane = Some(Pane::List { cursor: 0 });

        let world = FakeWorld::with(Vec::new());
        let folds = folds_of(&manager);
        manager
            .apply(Reaction::Adopt(session()), &folds, &world)
            .expect("the session is chosen");

        assert_eq!(manager.focus, 0, "the session the human is in is shown");
        assert_eq!(
            manager.held.len(),
            2,
            "a session this process holds is not opened twice",
        );
    }

    #[test]
    fn ctrl_n_names_a_new_session_and_enter_creates_it() {
        let mut manager = holding(vec![Fake::awaiting()]);

        assert_eq!(react(&mut manager, Key::New), Reaction::Idle);
        assert_eq!(
            manager.pane,
            Some(Pane::New {
                goal: String::new()
            }),
        );

        assert_eq!(
            react(&mut manager, Key::Enter),
            Reaction::Idle,
            "a goal of nothing creates no session",
        );

        for character in "count the quokkas".chars() {
            assert_eq!(react(&mut manager, Key::Char(character)), Reaction::Idle);
        }
        assert_eq!(react(&mut manager, Key::Backspace), Reaction::Idle);

        let created = "count the quokka";
        assert_eq!(
            react(&mut manager, Key::Enter),
            Reaction::Create(created.to_owned()),
        );

        let world = FakeWorld::with(Vec::new());
        let folds = folds_of(&manager);
        manager
            .apply(Reaction::Create(created.to_owned()), &folds, &world)
            .expect("the new session opens");

        assert_eq!(manager.held.len(), 2);
        assert_eq!(manager.focus, 1, "the human is in the session he just made");
        assert_eq!(manager.pane, None);
        let rows = manager.entries(&folds_of(&manager));
        assert_eq!(
            rows[1].goal.as_deref(),
            Some(created),
            "the new session carries the goal the human typed",
        );
    }

    #[test]
    fn a_goal_the_pane_cannot_hold_is_dropped_whole() {
        let mut manager = holding(vec![Fake::awaiting()]);
        manager.pane = Some(Pane::New {
            goal: "x".repeat(MAX_SPEECH_BYTES - 1),
        });

        assert_eq!(react(&mut manager, Key::Char('é')), Reaction::Idle);
        assert_eq!(
            manager.pane,
            Some(Pane::New {
                goal: "x".repeat(MAX_SPEECH_BYTES - 1)
            }),
            "a two-byte character must not be cut to fit the last byte",
        );

        assert_eq!(react(&mut manager, Key::Char('a')), Reaction::Idle);
        assert_eq!(
            manager.pane,
            Some(Pane::New {
                goal: format!("{}a", "x".repeat(MAX_SPEECH_BYTES - 1))
            }),
        );

        // Two bytes of room left, so a two-byte character still fits whole.
        let mut exact = holding(vec![Fake::awaiting()]);
        exact.pane = Some(Pane::New {
            goal: "x".repeat(MAX_SPEECH_BYTES - 2),
        });
        assert_eq!(react(&mut exact, Key::Char('é')), Reaction::Idle);
        assert_eq!(
            exact.pane,
            Some(Pane::New {
                goal: format!("{}é", "x".repeat(MAX_SPEECH_BYTES - 2))
            }),
            "a character that fits in the room left is taken",
        );
    }

    #[test]
    fn escape_closes_the_open_pane_and_leaves_the_rest_as_it_was() {
        let mut manager = holding(vec![Fake::awaiting()]);
        manager.pane = Some(Pane::New {
            goal: "count".to_owned(),
        });

        assert_eq!(react(&mut manager, Key::Escape), Reaction::Idle);
        assert_eq!(manager.pane, None);
        assert_eq!(manager.held.len(), 1);
        assert_eq!(manager.focus, 0);

        manager.pane = Some(Pane::List { cursor: 0 });
        assert_eq!(react(&mut manager, Key::List), Reaction::Idle);
        assert_eq!(
            manager.pane, None,
            "the key that opened the listing closes it too",
        );
    }

    #[test]
    fn the_panes_ignore_the_keys_that_are_not_theirs() {
        let mut manager = holding(vec![Fake::awaiting()]);
        manager.pane = Some(Pane::List { cursor: 0 });

        for key in [Key::Char('a'), Key::Backspace, Key::Next, Key::New] {
            assert_eq!(react(&mut manager, key), Reaction::Idle);
        }

        assert_eq!(manager.pane, Some(Pane::List { cursor: 0 }));
        assert_eq!(manager.held.len(), 1);
        assert_eq!(manager.focus, 0);
        assert_eq!(
            manager.held[0].input, "",
            "a character typed in the listing does not reach the conversation",
        );
    }

    #[test]
    fn ctrl_c_quits_from_the_conversation_and_from_every_pane() {
        let mut manager = holding(vec![Fake::awaiting()]);
        assert_eq!(react(&mut manager, Key::Interrupt), Reaction::Quit);

        for pane in [
            Pane::List { cursor: 0 },
            Pane::New {
                goal: "count".to_owned(),
            },
        ] {
            manager.pane = Some(pane);
            assert_eq!(react(&mut manager, Key::Interrupt), Reaction::Quit);
        }
    }

    #[test]
    fn a_reported_event_says_whether_the_human_is_leaving() {
        let mut manager = holding(vec![Fake::awaiting()]);
        let world = FakeWorld::with(Vec::new());
        let folds = folds_of(&manager);

        assert_eq!(
            manager
                .event(Event::Resize, &folds, &world)
                .expect("a resize asks for a frame"),
            Keep::Running,
        );
        assert_eq!(
            manager
                .event(Event::Key(Key::Char('a')), &folds, &world)
                .expect("a character is typed"),
            Keep::Running,
        );
        assert_eq!(manager.held[0].input, "a");
        assert_eq!(
            manager
                .event(Event::Key(Key::Interrupt), &folds, &world)
                .expect("the human leaves"),
            Keep::Leaving,
        );
    }

    #[test]
    fn the_state_row_carries_the_pane_s_own_phrase() {
        let mut manager = holding(vec![Fake::awaiting()]);
        manager.listed = vec![listing(others(2), Phase::Accepting, None)];
        let folds = folds_of(&manager);

        assert_eq!(manager.status(&folds), "your turn");

        manager.pane = Some(Pane::List { cursor: 0 });
        assert_eq!(manager.status(&folds), "2 sessions · * is held");

        manager.pane = Some(Pane::New {
            goal: String::new(),
        });
        assert_eq!(manager.status(&folds), "a new session");
    }

    #[test]
    fn the_spinner_advances_only_while_the_session_on_screen_works() {
        let mut manager = holding(vec![
            Fake::new(session(), &in_flight(turn_id(session(), 1))),
            Fake::awaiting(),
        ]);
        manager.focus = 0;
        let folds = folds_of(&manager);

        assert_eq!(manager.view(&folds).spinner, Some(0));
        manager.advance(&folds);
        assert_eq!(manager.view(&folds).spinner, Some(1));

        manager.focus = 1;
        let folds = folds_of(&manager);
        manager.advance(&folds);

        assert_eq!(
            manager.phase, 1,
            "a waiting session does not move the spinner"
        );
        assert_eq!(manager.view(&folds).spinner, None);
    }

    #[test]
    fn one_held_session_is_singular_in_the_listing_status() {
        let mut manager = holding(vec![Fake::awaiting()]);
        let folds = folds_of(&manager);

        manager.pane = Some(Pane::List { cursor: 0 });

        assert_eq!(manager.status(&folds), "1 session · * is held");
    }

    #[test]
    fn the_frame_carries_the_session_the_line_the_scroll_and_the_pane() {
        let mut manager = holding(vec![Fake::awaiting()]);
        manager.held[0].input = "hello".to_owned();
        manager.held[0].scroll = 3;
        let folds = folds_of(&manager);

        let view = manager.view(&folds);

        assert_eq!(view.session, session());
        assert_eq!(view.input, "hello");
        assert_eq!(view.scroll, 3);
        assert_eq!(view.status, "your turn");
        assert_eq!(view.background, None);
        assert_eq!(view.sheet, None);

        manager.pane = Some(Pane::New {
            goal: "count".to_owned(),
        });
        let view = manager.view(&folds);

        assert_eq!(
            view.sheet,
            Some(crate::port::Sheet::New {
                goal: "count".to_owned()
            }),
        );
    }

    #[test]
    fn a_session_this_process_holds_is_named_by_its_own_engine() {
        let manager = holding(vec![Fake::new(others(4), &awaiting())]);

        assert_eq!(manager.held[0].engine.session(), others(4));
        assert_eq!(manager.index_of(others(4)), Some(0));
        assert_eq!(manager.index_of(session()), None);
    }

    #[test]
    fn an_unopened_session_that_owes_the_human_nothing_is_waiting() {
        assert!(matches!(
            mode(&fold_of(&[Record::Task(task())])),
            Mode::Waiting
        ));
    }

    proptest! {
        #[test]
        fn typing_never_carries_the_line_past_its_ceiling(typed in prop::collection::vec(any::<char>(), 0..64)) {
            let mut manager = holding(vec![Fake::awaiting()]);

            for character in typed {
                let _ = react(&mut manager, Key::Char(character));
            }

            prop_assert!(
                manager.held[0].input.len() <= MAX_SPEECH_BYTES,
                "the input {:?} holds {} bytes", manager.held[0].input, manager.held[0].input.len(),
            );
        }
    }
}
