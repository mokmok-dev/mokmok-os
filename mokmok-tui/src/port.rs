//! The ports: what the core needs from one session, from the log, and where a frame goes.

use std::future::Future;
use std::path::PathBuf;

use mokmok_agent_runtime::{Progress, Transcript};
use mokmok_protocol::SessionID;
use mokmok_state_machine::Phase;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::Error;

/// Everything the core needs from one session: its past, one human line, one bounded step.
///
/// The port speaks the domain's language — a [`Transcript`], a [`Progress`] — so an implementation
/// supplies the world and decides nothing. It hands out values rather than borrows, because the fold
/// it hands over must outlive the mutable step that follows it.
pub trait Engine {
    /// The session this engine works in.
    fn session(&self) -> SessionID;

    /// The session's whole durable past, folded.
    ///
    /// # Errors
    ///
    /// Returns the journal's failure when the history cannot be read or does not fold.
    fn fold(&self) -> Result<Transcript, Error>;

    /// Commits one human line as the input of the session's next turn.
    ///
    /// # Errors
    ///
    /// Returns the journal's failure, and its refusal when the session is not waiting for a line.
    fn speak(
        &mut self,
        line: &str,
    ) -> Result<(), Error>;

    /// Performs at most one bounded step of the loop.
    ///
    /// The future is not `Send`: a streamed step may hold the line being typed and a callback that
    /// writes to it, and a session worker owns both on its own thread.
    ///
    /// A step dispatched while [`busy`](Self::busy) is true is not performed: the dispatch answers
    /// [`Progress::Awaiting`] and the work already in flight reports through
    /// [`drain`](Self::drain).
    ///
    /// # Errors
    ///
    /// Returns the journal's, the provider's, or the tools' own failure.
    fn step(&mut self) -> impl Future<Output = Result<Progress, Error>>;

    /// Performs one bounded step, forwarding each prose fragment the model emits while it streams.
    ///
    /// The default takes the step whole and forwards nothing, which is what an engine with no
    /// stream to report does. The observer is not `Send`: an engine on the loop's thread holds the
    /// screen, and one on a worker holds the channel it reports through, so both stay on the thread
    /// that will call the observer.
    ///
    /// # Errors
    ///
    /// Returns the journal's, the provider's, or the tools' own failure.
    fn step_watched<'a>(
        &'a mut self,
        _watch: &'a mut dyn FnMut(&str),
    ) -> impl Future<Output = Result<Progress, Error>> + 'a {
        async move { self.step().await }
    }

    /// Re-opens the session, settling what a dead process left in flight.
    ///
    /// # Errors
    ///
    /// Returns the store's failure when the session cannot be re-opened.
    fn reopen(&mut self) -> Result<(), Error>;

    /// Whether a step this engine dispatched has not reported back yet.
    ///
    /// An engine that runs its step inline is never busy, because the step returned before the
    /// caller could ask. One that runs its step on another thread answers this from the dispatch
    /// it made, so the loop can keep reading the terminal while that step works.
    fn busy(&self) -> bool {
        false
    }

    /// Takes everything a step reported since the last call, without waiting.
    ///
    /// An engine that runs its step inline reports nothing here: its result already went back
    /// through [`step`](Self::step). One that runs its step elsewhere reports the fragments it
    /// streamed and the result it reached.
    fn drain(&mut self) -> Vec<Update> {
        Vec::new()
    }
}

/// What a step working elsewhere reported, in the order it reported it.
#[derive(Debug)]
pub enum Update {
    /// A prose fragment the model emitted while the step streams.
    Fragment(String),
    /// The step returned, or the engine could not finish dispatching it.
    Stepped(Result<Progress, Error>),
    /// The session's world failed somewhere a step did not cover, such as the line being spoken.
    Failed(Error),
}

/// One session the log holds, as a chooser shows it.
///
/// The choice of fields is the choice of what a human needs before he opens a session he cannot see:
/// which one it is, where it stands, and what it was asked to do. Nothing here is the session's own
/// current fold, which only an engine can answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Listing {
    /// The session's identity, which is also the stream its history lives in.
    pub session: SessionID,
    /// The phase its own facts fold to.
    pub phase: Phase,
    /// The goal its task named, when it recorded one.
    pub goal: Option<String>,
    /// The checkout that task named, when it recorded one.
    pub checkout: Option<PathBuf>,
}

/// The sessions the log holds, and how to open one.
pub trait Roster {
    /// The engine one session runs on.
    type Engine: Engine;

    /// The sessions the log holds, the one written most recently first.
    ///
    /// # Errors
    ///
    /// Returns the log's failure when its listing cannot be read.
    fn listed(&self) -> Result<Vec<Listing>, Error>;

    /// Opens a session the log already holds, resuming it from the task it recorded.
    ///
    /// # Errors
    ///
    /// Returns the errors of opening the session's world: its log, its tool, and its provider.
    fn resume(
        &self,
        listing: &Listing,
    ) -> Result<Self::Engine, Error>;

    /// Opens a session that does not exist yet, to work on `goal`.
    ///
    /// # Errors
    ///
    /// Returns the errors of opening the session's world: its log, its tool, and its provider.
    fn create(
        &self,
        goal: &str,
    ) -> Result<Self::Engine, Error>;
}

/// Where the conversation is drawn and where keys come from.
///
/// Synchronous on purpose: the step of a session runs on the caller's thread, so the terminal is
/// asked what it holds rather than waited on, and the sessions whose turn it is move in between.
pub trait Screen {
    /// Whether the terminal has something for the conversation right now, taking nothing.
    ///
    /// # Errors
    ///
    /// Returns the terminal's failure when a reported event cannot be read.
    fn poll(&mut self) -> Result<Option<Event>, Error>;

    /// Waits for the one thing the terminal reported next.
    ///
    /// # Errors
    ///
    /// Returns the terminal's failure when the event cannot be read.
    fn event(&mut self) -> Result<Event, Error>;

    /// Draws one frame.
    ///
    /// # Errors
    ///
    /// Returns the terminal's failure when the frame cannot be drawn.
    fn draw(
        &mut self,
        view: &View,
    ) -> Result<(), Error>;
}

/// What the conversation listens for. The adapter drops every key not named here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    /// One character typed into the line.
    Char(char),
    /// The line is submitted.
    Enter,
    /// The last character of the line is removed.
    Backspace,
    /// The transcript scrolls one line back.
    Up,
    /// The transcript scrolls one line forward.
    Down,
    /// The next session this process holds is shown.
    Next,
    /// The listing of the log's sessions is opened, or closed when it is open.
    List,
    /// The pane that names a new session's goal is opened.
    New,
    /// The open pane is closed.
    Escape,
    /// The human leaves, with every session as it stands.
    Interrupt,
}

/// One thing the terminal reported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    /// A key was pressed.
    Key(Key),
    /// The terminal was resized, so the frame is redrawn.
    Resize,
}

/// One line of the conversation.
///
/// The cases are the things one turn can hold — what the human said, what the agent said, the one
/// command it ran, what the finder answered, and a turn it could not finish with the reason why —
/// so a renderer cannot invent a sixth.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Line {
    /// What the human said.
    Human(String),
    /// What the agent said.
    Agent(String),
    /// The one command the agent ran, and what came of it.
    Work {
        /// The command string the agent asked to run.
        command: String,
        /// What the command produced, as the model was shown it.
        observation: String,
    },
    /// What the agent asked the finder, and what it answered.
    ///
    /// A find is its own line kind rather than a [`Line::Work`]: nothing ran, and a `command` field
    /// would tell the human a program had.
    Find { query: String, observation: String },
    /// An edit the agent asked for, and what came of it.
    Edit { observation: String },
    /// A delegation to a child session, with the goal it was given and what it answered.
    Delegate {
        child_session: SessionID,
        goal: String,
        reply: String,
    },
    /// A turn the agent could not finish, and why.
    Trouble(String),
}

/// What the input row starts with, so the line being typed and the goal being named are told apart.
const INPUT_PREFIX: &str = "> ";

const GOAL_PREFIX: &str = "goal> ";

const HUMAN_GUTTER: &str = "you    ";

const AGENT_GUTTER: &str = "agent  ";

const TROUBLE_GUTTER: &str = "!      ";

// Seven columns, so a continuation row aligns under the first row's text.
const CONTINUATION_PREFIX: &str = "       ";

const COMMAND_PREFIX: &str = "  $ ";

const QUERY_PREFIX: &str = "  ? ";

const EDIT_PREFIX: &str = "  ✎ ";

const DELEGATE_PREFIX: &str = "  ⤷ ";

const OBSERVATION_PREFIX: &str = "    ";

impl std::fmt::Display for Line {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        let rows: Vec<String> = match self {
            // The replay is plain text written after the terminal is handed back, with no row limit,
            // and it is the record the human takes away, so it carries every observation row the
            // frame trims to its four-row budget.
            Self::Work {
                command,
                observation,
            } => {
                let mut rows = command_row(command, usize::MAX);
                rows.extend(
                    observation
                        .lines()
                        .flat_map(|line| observation_row(line, usize::MAX)),
                );
                rows
            },
            Self::Find { query, observation } => {
                let mut rows = query_row(query, usize::MAX);
                rows.extend(
                    observation
                        .lines()
                        .flat_map(|line| observation_row(line, usize::MAX)),
                );
                rows
            },
            Self::Edit { observation } => {
                let mut rows = edit_row(observation, usize::MAX);
                rows.extend(
                    observation
                        .lines()
                        .skip(1)
                        .flat_map(|line| observation_row(line, usize::MAX)),
                );
                rows
            },
            Self::Delegate {
                child_session,
                goal,
                reply,
            } => {
                let mut rows = delegate_row(goal, usize::MAX);
                rows.extend(observation_row(
                    &format!("child: {}", child_session.as_uuid()),
                    usize::MAX,
                ));
                rows.extend(
                    reply
                        .lines()
                        .flat_map(|line| observation_row(line, usize::MAX)),
                );
                rows
            },
            other => rows_of(other, usize::MAX),
        };

        formatter.write_str(&rows.join("\n"))
    }
}

fn rows_of(
    line: &Line,
    columns: usize,
) -> Vec<String> {
    match line {
        Line::Human(text) => prose_rows(HUMAN_GUTTER, text, columns),
        Line::Agent(text) => prose_rows(AGENT_GUTTER, text, columns),
        Line::Trouble(text) => prose_rows(TROUBLE_GUTTER, text, columns),
        Line::Work {
            command,
            observation,
        } => {
            let mut rows = command_row(command, columns);

            // Four fixed rows is the frame's whole budget, so the first observation line is all it
            // can spare for the reason a command failed.
            if let Some(outcome) = observation.lines().next().filter(|first| !first.is_empty()) {
                rows.extend(observation_row(outcome, columns));
            }

            rows
        },
        Line::Find { query, observation } => {
            let mut rows = query_row(query, columns);

            if let Some(outcome) = observation.lines().next().filter(|first| !first.is_empty()) {
                rows.extend(observation_row(outcome, columns));
            }

            rows
        },
        Line::Edit { observation } => {
            let mut rows = Vec::new();

            if let Some(first) = observation.lines().next().filter(|first| !first.is_empty()) {
                rows.extend(edit_row(first, columns));
            }
            if let Some(outcome) = observation
                .lines()
                .nth(1)
                .filter(|second| !second.is_empty())
            {
                rows.extend(observation_row(outcome, columns));
            }

            rows
        },
        Line::Delegate {
            child_session,
            goal,
            reply,
        } => {
            let mut rows = delegate_row(goal, columns);
            rows.extend(observation_row(
                &format!("child: {}", child_session.as_uuid()),
                columns,
            ));
            if let Some(first) = reply.lines().next().filter(|first| !first.is_empty()) {
                rows.extend(observation_row(first, columns));
            }

            rows
        },
    }
}

fn command_row(
    command: &str,
    columns: usize,
) -> Vec<String> {
    prefixed(COMMAND_PREFIX, &escaped(command), columns)
}

fn query_row(
    query: &str,
    columns: usize,
) -> Vec<String> {
    prefixed(QUERY_PREFIX, &escaped(query), columns)
}

fn edit_row(
    observation: &str,
    columns: usize,
) -> Vec<String> {
    prefixed(EDIT_PREFIX, &escaped(observation), columns)
}

fn observation_row(
    line: &str,
    columns: usize,
) -> Vec<String> {
    prefixed(OBSERVATION_PREFIX, &escaped(line), columns)
}

fn delegate_row(
    goal: &str,
    columns: usize,
) -> Vec<String> {
    prefixed(
        DELEGATE_PREFIX,
        &format!("delegate: {}", escaped(goal)),
        columns,
    )
}

/// One speaker's text as rows: the gutter leads the first line, seven spaces each later one.
///
/// A single trailing line break ends the last line rather than opening an empty one, so it draws
/// no blank row.
fn prose_rows(
    gutter: &str,
    text: &str,
    columns: usize,
) -> Vec<String> {
    let body = text.strip_suffix('\n').unwrap_or(text);

    body.split('\n')
        .enumerate()
        .flat_map(|(index, line)| {
            let prefix = if index == 0 {
                gutter
            } else {
                CONTINUATION_PREFIX
            };
            // A carriage return before a line break is part of the break, not a character to draw.
            let line = line.strip_suffix('\r').unwrap_or(line);

            prefixed(prefix, &escaped(line), columns)
        })
        .collect()
}

/// `text` behind `prefix`, with every later row indented past the prefix so it stays under the text
/// it continues.
fn prefixed(
    prefix: &str,
    text: &str,
    columns: usize,
) -> Vec<String> {
    let hanging = prefix.width();
    let content = columns.saturating_sub(hanging).max(1);
    let indent = " ".repeat(hanging);

    wrapped(text, content)
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let lead = if index == 0 { prefix } else { indent.as_str() };

            format!("{lead}{row}")
        })
        .collect()
}

/// `text` broken into rows of at most `columns`, measured as the terminal draws them: a wide
/// character takes two columns and a combining one none.
fn wrapped(
    text: &str,
    columns: usize,
) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut rows = Vec::new();
    let mut rest = text;

    while let Some(first) = rest.chars().next() {
        // The row's first character is taken whatever its width, so a row always advances the text
        // and a wide character in a one-column row still leaves the rest for the next row.
        let mut row = String::new();
        let mut used = first.width().unwrap_or(0);
        let mut space = None;
        row.push(first);

        for character in rest[first.len_utf8()..].chars() {
            let width = character.width().unwrap_or(0);

            if used + width > columns {
                break;
            }

            used += width;
            row.push(character);

            if character == ' ' {
                space = Some(row.len());
            }
        }

        if row.len() == rest.len() {
            rows.push(rest.to_owned());
            break;
        }

        let at = space.unwrap_or(row.len());

        rows.push(row[..at].trim_end_matches(' ').to_owned());
        rest = &rest[at..];
    }

    rows
}

/// Replaces every control character so a drawn row holds the text the shell received.
///
/// A terminal row cannot hold a control character and the renderer drops the ones it cannot place,
/// so a raw newline would join a program's lines and show the human a command that never ran.
fn escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for character in text.chars() {
        match character {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control.is_control() => escaped.push('?'),
            other => escaped.push(other),
        }
    }

    escaped
}

/// One frame, as data: the renderer lays it out and decides nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct View {
    /// The session's name, so the human can read the conversation back with `mokmokd status`.
    pub session: SessionID,
    /// Every line of the conversation, oldest first.
    pub lines: Vec<Line>,
    /// The line being typed.
    pub input: String,
    /// One phrase for the session's state and whose turn it is.
    pub status: String,
    /// How many drawn rows back from the newest the transcript is scrolled.
    pub scroll: u16,
    /// One phrase for the sessions whose turn it is besides the one on screen, when any are.
    ///
    /// `None` when nothing else is moving, so the frame of a single session is the frame it was.
    pub background: Option<String>,
    /// The pane that replaces the transcript, when the human opened one.
    pub sheet: Option<Sheet>,
}

/// A pane the human opened over the conversation.
///
/// The cases are the two things he can be doing besides typing a line — choosing a session, and
/// naming one that does not exist yet — so the screen cannot invent a third.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Sheet {
    /// The sessions this process holds and the ones the log holds, with the cursor on one row.
    List {
        /// The rows, in the order they are drawn.
        rows: Vec<Row>,
        /// Which row the cursor is on.
        cursor: usize,
    },
    /// The goal of a session that does not exist yet, as it is being typed.
    New {
        /// The goal so far.
        goal: String,
    },
}

/// One row of the listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    /// The session the row names.
    pub session: SessionID,
    /// The goal the session recorded, when it recorded one.
    pub goal: Option<String>,
    /// One phrase for the session's state, the same one the header shows for the session on screen.
    pub status: String,
    /// Whether this process holds the session's engine.
    pub held: bool,
    /// Whether it is the session the human is in.
    pub focused: bool,
}

const NO_GOAL: &str = "(no goal recorded)";

// Two columns, so the cursor's row and the session the human is in can be told apart at a glance.
const CURSOR_MARK: &str = "> ";

const NO_CURSOR_MARK: &str = "  ";

const HELD_MARK: &str = "* ";

const NO_HELD_MARK: &str = "  ";

const COLUMN_GAP: &str = "  ";

fn drawn_row(
    cursor: bool,
    row: &Row,
) -> String {
    let cursor = if cursor { CURSOR_MARK } else { NO_CURSOR_MARK };
    let held = if row.held { HELD_MARK } else { NO_HELD_MARK };
    let goal = row.goal.as_deref().unwrap_or(NO_GOAL);

    format!(
        "{cursor}{held}{}{COLUMN_GAP}{}{COLUMN_GAP}{goal}",
        row.session.as_uuid(),
        row.status,
    )
}

/// The line being typed, laid out in a pane `width` columns wide.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Typing {
    /// The rows to draw, the first behind the prompt and the rest indented past it.
    pub rows: Vec<String>,
    /// Which of the rows holds the cursor.
    pub row: usize,
    /// The column the next character takes.
    pub column: usize,
}

impl View {
    /// The rows a transcript `width` columns wide and `height` rows tall shows, newest last.
    ///
    /// A pure function of the view and the two things the adapter knows, so which rows are on screen
    /// and where each one breaks are decided here and tested without a terminal. Rows are counted
    /// after they are wrapped, so the scroll and the window move by drawn rows.
    #[must_use]
    pub fn visible(
        &self,
        width: usize,
        height: usize,
    ) -> Vec<String> {
        let rows: Vec<String> = self
            .lines
            .iter()
            .flat_map(|line| rows_of(line, width))
            .collect();
        let end = rows.len().saturating_sub(usize::from(self.scroll));
        let start = end.saturating_sub(height);

        rows[start..end].to_vec()
    }

    /// The rows the body shows: the listing when the human opened it, and the transcript otherwise.
    ///
    /// Naming a session's goal leaves the conversation behind the pane, because the goal is usually a
    /// reading of what is already on screen. The listing's columns are a table rather than prose, so
    /// its rows are left to the renderer to truncate.
    #[must_use]
    pub fn body(
        &self,
        width: usize,
        height: usize,
    ) -> Vec<String> {
        match &self.sheet {
            Some(Sheet::List { rows, cursor }) => {
                let drawn: Vec<String> = rows
                    .iter()
                    .enumerate()
                    .map(|(index, row)| drawn_row(index == *cursor, row))
                    .collect();

                window(&drawn, *cursor, height)
            },
            _ => self.visible(width, height),
        }
    }

    /// The line being typed, laid out and placed for a pane `width` columns wide.
    ///
    /// A character that fills the last column leaves the cursor on a fresh row, which is where the
    /// terminal puts it. A pane narrower than the prompt keeps the cursor on its last column.
    #[must_use]
    pub fn typing(
        &self,
        width: usize,
    ) -> Typing {
        let (prompt, text) = match &self.sheet {
            Some(Sheet::List { .. }) => {
                return Typing {
                    rows: vec![String::new()],
                    row: 0,
                    column: 0,
                };
            },
            Some(Sheet::New { goal }) => (GOAL_PREFIX, goal.as_str()),
            None => (INPUT_PREFIX, self.input.as_str()),
        };

        let hanging = prompt.width();
        let mut rows = prefixed(prompt, text, width);
        let mut row = rows.len() - 1;
        let mut column = rows[row].width();

        if column >= width {
            rows.push(" ".repeat(hanging));
            row += 1;
            column = hanging;
        }

        column = column.min(width.saturating_sub(1));

        Typing { rows, row, column }
    }
}

fn window(
    rows: &[String],
    cursor: usize,
    height: usize,
) -> Vec<String> {
    let start = cursor
        .saturating_sub(height / 2)
        .min(rows.len().saturating_sub(height));

    rows[start..].iter().take(height).cloned().collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use uuid::Uuid;

    use super::{Line, Row, Sheet, Typing, View, edit_row};
    use mokmok_protocol::SessionID;

    fn view(
        lines: Vec<Line>,
        scroll: u16,
    ) -> View {
        View {
            session: SessionID::from_uuid(Uuid::from_u128(1)),
            lines,
            input: String::new(),
            status: String::new(),
            scroll,
            background: None,
            sheet: None,
        }
    }

    fn named(seed: u128) -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(seed))
    }

    fn row(
        session: SessionID,
        goal: Option<&str>,
        held: bool,
    ) -> Row {
        Row {
            session,
            goal: goal.map(str::to_owned),
            status: "your turn".to_owned(),
            held,
            focused: false,
        }
    }

    fn listing(
        count: usize,
        cursor: usize,
        goals: &[Option<&str>],
    ) -> View {
        let rows = (0..count)
            .map(|index| {
                row(
                    named(u128::try_from(index).unwrap() + 1),
                    goals.get(index).copied().flatten(),
                    index == 0,
                )
            })
            .collect();

        View {
            sheet: Some(Sheet::List { rows, cursor }),
            ..view(Vec::new(), 0)
        }
    }

    #[test]
    fn the_listing_draws_the_cursor_the_held_mark_and_the_goal() {
        let view = listing(
            2,
            1,
            &[Some("hold a conversation"), Some("count the quokkas")],
        );

        assert_eq!(
            view.body(usize::MAX, 10),
            vec![
                "  * 00000000-0000-0000-0000-000000000001  your turn  hold a conversation"
                    .to_owned(),
                ">   00000000-0000-0000-0000-000000000002  your turn  count the quokkas".to_owned(),
            ],
            "the cursor row, the held row, and each goal are drawn",
        );
    }

    #[test]
    fn a_session_that_recorded_no_goal_says_so_in_its_row() {
        let view = listing(1, 0, &[None]);

        assert!(
            view.body(usize::MAX, 1)[0].ends_with("(no goal recorded)"),
            "a row with no goal says there is none: {:?}",
            view.body(usize::MAX, 1),
        );
    }

    #[test]
    fn the_listing_holds_the_cursor_row_in_a_pane_shorter_than_the_rows() {
        let goals = [None; 5];
        let rows = |cursor| {
            listing(5, cursor, &goals)
                .body(usize::MAX, 3)
                .iter()
                .map(|row| row.chars().next().unwrap())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            rows(0),
            vec!['>', ' ', ' '],
            "the cursor at the top stays there"
        );
        assert_eq!(
            rows(4),
            vec![' ', ' ', '>'],
            "the cursor at the bottom stays there",
        );
        assert_eq!(
            rows(2),
            vec![' ', '>', ' '],
            "the cursor in the middle is centred"
        );
        assert_eq!(
            listing(5, 0, &goals).body(usize::MAX, 5).len(),
            5,
            "a pane that fits shows every row"
        );
        assert!(
            listing(5, 0, &goals).body(usize::MAX, 0).is_empty(),
            "a pane with no room shows no row",
        );

        // An even pane height, where the rows kept above the cursor are not the rows between the top
        // of the window and the cursor.
        let even = listing(6, 3, &[None; 6]).body(usize::MAX, 4);

        assert_eq!(even.len(), 4);
        assert_eq!(
            even.iter()
                .map(|row| row.chars().next().expect("a marker"))
                .collect::<Vec<_>>(),
            vec![' ', ' ', '>', ' '],
            "a pane four rows tall keeps two rows above the cursor",
        );
        assert!(
            even[0].contains("00000000-0000-0000-0000-000000000002"),
            "the window starts two rows above the cursor's own row: {:?}",
            even[0],
        );
    }

    #[test]
    fn naming_a_goal_leaves_the_conversation_behind_the_pane() {
        let conversation = view(vec![Line::Human("hello".to_owned())], 0);
        let naming = View {
            sheet: Some(Sheet::New {
                goal: "count the quokkas".to_owned(),
            }),
            ..conversation.clone()
        };

        assert_eq!(
            naming.body(usize::MAX, 10),
            conversation.body(usize::MAX, 10),
            "the pane that names a goal draws the conversation it is about",
        );
    }

    #[test]
    fn visible_draws_the_gutter_and_the_work_outcome() {
        let view = view(
            vec![
                Line::Human("hello".to_owned()),
                Line::Work {
                    command: "ls".to_owned(),
                    observation: "out.txt\nmore\n".to_owned(),
                },
                Line::Agent("bye".to_owned()),
            ],
            0,
        );

        assert_eq!(
            view.visible(usize::MAX, 10),
            vec![
                "you    hello".to_owned(),
                "  $ ls".to_owned(),
                "    out.txt".to_owned(),
                "agent  bye".to_owned(),
            ],
        );
        assert_eq!(view.visible(usize::MAX, 0), Vec::<String>::new());
    }

    #[test]
    fn a_find_draws_its_query_and_its_outcome_under_a_gutter_of_its_own() {
        let view = view(
            vec![
                Line::Find {
                    query: "quokka".to_owned(),
                    observation: "indexed=2 hits=1\n1.500  /checkout/notes.txt\n".to_owned(),
                },
                Line::Agent("found it".to_owned()),
            ],
            0,
        );

        assert_eq!(
            view.visible(usize::MAX, 10),
            vec![
                "  ? quokka".to_owned(),
                "    indexed=2 hits=1".to_owned(),
                "agent  found it".to_owned(),
            ],
            "a find must not draw the command's gutter",
        );
    }

    #[test]
    fn an_edit_draws_its_first_two_lines_with_the_edit_gutter() {
        let line = Line::Edit {
            observation: "edit applied to src/x.rs\n1 bytes became 1 bytes\n".to_owned(),
        };

        assert_eq!(
            view(vec![line.clone()], 0).visible(usize::MAX, 2),
            vec![
                "  ✎ edit applied to src/x.rs".to_owned(),
                "    1 bytes became 1 bytes".to_owned(),
            ],
            "an edit's first two non-empty lines appear in the frame",
        );

        let replayed = format!("{line}");
        assert!(
            replayed.contains("edit applied to src/x.rs")
                && replayed.contains("1 bytes became 1 bytes"),
            "the replay carries every line of the edit: {replayed:?}"
        );
    }

    #[test]
    fn edit_row_renders_the_exact_gutter() {
        assert_eq!(
            edit_row("applied to src/x.rs", usize::MAX),
            vec!["  ✎ applied to src/x.rs".to_owned()],
        );
    }

    #[test]
    fn a_delegate_line_renders_its_goal_child_and_reply() {
        let session = SessionID::from_uuid(Uuid::from_u128(2));
        let line = Line::Delegate {
            child_session: session,
            goal: "find the license".to_owned(),
            reply: "MIT license\nsummary here".to_owned(),
        };

        assert_eq!(
            view(vec![line.clone()], 0).visible(usize::MAX, 3),
            vec![
                "  ⤷ delegate: find the license".to_owned(),
                format!("    child: {}", session.as_uuid()),
                "    MIT license".to_owned(),
            ],
            "the frame shows the goal, the child session, and the first reply line",
        );

        let replayed = format!("{line}");
        assert!(
            replayed.contains("delegate: find the license")
                && replayed.contains(&session.as_uuid().to_string())
                && replayed.contains("summary here"),
            "the replay carries every line: {replayed:?}",
        );
    }

    #[test]
    fn the_replay_carries_every_find_row_and_the_frame_carries_the_first() {
        let line = Line::Find {
            query: "quokka".to_owned(),
            observation: "indexed=2 hits=1\n1.500  /checkout/notes.txt\n".to_owned(),
        };

        let replayed = format!("{line}");
        assert!(
            replayed.contains("notes.txt"),
            "the replay must carry every row of the find's answer: {replayed:?}",
        );
        assert_eq!(
            view(vec![line], 0).visible(usize::MAX, 2),
            vec!["  ? quokka".to_owned(), "    indexed=2 hits=1".to_owned()],
        );
    }

    #[test]
    fn a_reply_with_line_breaks_draws_one_row_per_line() {
        let view = view(
            vec![Line::Agent(
                "fib.txt holds:\n1, 1, 2, 3, 5\nAnything else?".to_owned(),
            )],
            0,
        );

        assert_eq!(
            view.visible(usize::MAX, 3),
            vec![
                "agent  fib.txt holds:".to_owned(),
                "       1, 1, 2, 3, 5".to_owned(),
                "       Anything else?".to_owned(),
            ],
            "each line of the reply is its own row, the later rows aligned under the gutter",
        );
        assert_eq!(
            view.visible(usize::MAX, 2),
            vec![
                "       1, 1, 2, 3, 5".to_owned(),
                "       Anything else?".to_owned(),
            ],
            "the window arithmetic counts the reply's three rows, not its one line",
        );
    }

    #[test]
    fn a_reply_of_one_line_stays_one_row() {
        let one = view(vec![Line::Agent("one line".to_owned())], 0);

        assert_eq!(
            one.visible(usize::MAX, 1),
            vec!["agent  one line".to_owned()]
        );

        let ended = view(vec![Line::Agent("one line\n".to_owned())], 0);

        assert_eq!(
            ended.visible(usize::MAX, 2),
            vec!["agent  one line".to_owned()],
            "the trailing line break ends the line instead of drawing a blank row after it",
        );
    }

    #[test]
    fn a_row_wider_than_the_pane_breaks_at_a_space_and_indents_the_next_row() {
        let view = view(vec![Line::Agent("alpha bravo charlie".to_owned())], 0);

        assert_eq!(
            view.visible(16, 3),
            vec![
                "agent  alpha".to_owned(),
                "       bravo".to_owned(),
                "       charlie".to_owned(),
            ],
            "a word that does not fit moves whole to the next row",
        );
        assert_eq!(
            view.visible(16, 2),
            vec!["       bravo".to_owned(), "       charlie".to_owned()],
            "the window counts the rows a line was wrapped over",
        );
    }

    #[test]
    fn a_word_wider_than_the_pane_breaks_inside_it() {
        let view = view(vec![Line::Agent("abcdefghij".to_owned())], 0);

        assert_eq!(
            view.visible(11, 3),
            vec![
                "agent  abcd".to_owned(),
                "       efgh".to_owned(),
                "       ij".to_owned(),
            ],
        );
    }

    #[test]
    fn a_wide_character_takes_two_columns_and_is_never_split() {
        let view = view(vec![Line::Agent("あああ".to_owned())], 0);

        assert_eq!(
            view.visible(11, 2),
            vec!["agent  ああ".to_owned(), "       あ".to_owned()],
            "two wide characters fill the four columns left beside the gutter",
        );
    }

    #[test]
    fn a_wide_character_alone_in_a_one_column_row_is_drawn_whole() {
        let view = view(vec![Line::Agent("ああ".to_owned())], 0);

        assert_eq!(
            view.visible(8, 2),
            vec!["agent  あ".to_owned(), "       あ".to_owned()],
            "a row narrower than the character takes it whole rather than nothing",
        );
    }

    #[test]
    fn a_work_outcome_wraps_its_first_line_under_its_own_gutter() {
        let view = view(
            vec![Line::Work {
                command: "ls".to_owned(),
                observation: "alpha bravo charlie".to_owned(),
            }],
            0,
        );

        assert_eq!(
            view.visible(16, 3),
            vec![
                "  $ ls".to_owned(),
                "    alpha bravo".to_owned(),
                "    charlie".to_owned(),
            ],
            "the frame keeps the command row and wraps the observation's first line",
        );
    }

    #[test]
    fn typing_wraps_the_line_and_keeps_the_cursor_inside_the_pane() {
        let view = View {
            input: "abcdefghij".to_owned(),
            ..view(Vec::new(), 0)
        };

        assert_eq!(
            view.typing(10),
            Typing {
                rows: vec!["> abcdefgh".to_owned(), "  ij".to_owned()],
                row: 1,
                column: 4,
            },
        );
    }

    #[test]
    fn typing_that_fills_the_last_column_leaves_the_cursor_on_a_fresh_row() {
        let view = View {
            input: "abcdefgh".to_owned(),
            ..view(Vec::new(), 0)
        };

        assert_eq!(
            view.typing(10),
            Typing {
                rows: vec!["> abcdefgh".to_owned(), "  ".to_owned()],
                row: 1,
                column: 2,
            },
            "a cursor at the pane's own width would be off the screen",
        );
    }

    #[test]
    fn typing_counts_a_wide_character_as_two_columns() {
        let wide = View {
            input: "日本語".to_owned(),
            ..view(Vec::new(), 0)
        };

        assert_eq!(
            wide.typing(80),
            Typing {
                rows: vec!["> 日本語".to_owned()],
                row: 0,
                column: 8,
            },
        );

        let empty = view(Vec::new(), 0).typing(80);
        assert_eq!(
            (empty.row, empty.column),
            (0, 2),
            "an empty line leaves the cursor past the prompt",
        );
    }

    #[test]
    fn typing_a_goal_wraps_and_places_the_cursor_past_its_own_prompt() {
        let naming = View {
            sheet: Some(Sheet::New {
                goal: "count the quokkas".to_owned(),
            }),
            ..view(Vec::new(), 0)
        };

        assert_eq!(
            naming.typing(80),
            Typing {
                rows: vec!["goal> count the quokkas".to_owned()],
                row: 0,
                column: 23,
            },
        );

        assert_eq!(
            naming.typing(14),
            Typing {
                rows: vec![
                    "goal> count".to_owned(),
                    "      the".to_owned(),
                    "      quokkas".to_owned(),
                ],
                row: 2,
                column: 13,
            },
        );
    }

    #[test]
    fn a_pane_narrower_than_its_prompt_keeps_the_cursor_on_its_last_column() {
        let naming = View {
            sheet: Some(Sheet::New {
                goal: "abc".to_owned(),
            }),
            ..view(Vec::new(), 0)
        };

        assert_eq!(
            naming.typing(1),
            Typing {
                rows: vec![
                    "goal> a".to_owned(),
                    "      b".to_owned(),
                    "      c".to_owned(),
                    "      ".to_owned(),
                ],
                row: 3,
                column: 0,
            },
        );
    }

    #[test]
    fn the_listing_leaves_the_input_row_blank() {
        let view = listing(1, 0, &[None]);

        assert_eq!(
            view.typing(80),
            Typing {
                rows: vec![String::new()],
                row: 0,
                column: 0,
            },
        );
    }

    #[test]
    fn a_human_line_with_line_breaks_is_drawn_the_same_way() {
        let view = view(
            vec![Line::Human(
                "fib.txt holds:\n1, 1, 2, 3, 5\nAnything else?".to_owned(),
            )],
            0,
        );

        assert_eq!(
            view.visible(usize::MAX, 3),
            vec![
                "you    fib.txt holds:".to_owned(),
                "       1, 1, 2, 3, 5".to_owned(),
                "       Anything else?".to_owned(),
            ],
        );
    }

    #[test]
    fn visible_keeps_the_newest_rows_and_windows_by_rows() {
        let view = view(
            vec![
                Line::Human("first".to_owned()),
                Line::Work {
                    command: "ls".to_owned(),
                    observation: "out.txt".to_owned(),
                },
                Line::Agent("one\ntwo\nthree".to_owned()),
            ],
            0,
        );

        assert_eq!(
            view.visible(usize::MAX, 4),
            vec![
                "    out.txt".to_owned(),
                "agent  one".to_owned(),
                "       two".to_owned(),
                "       three".to_owned(),
            ],
            "a reply's continuation rows count toward a window shorter than the transcript",
        );
        assert_eq!(
            view.visible(usize::MAX, 2),
            vec!["       two".to_owned(), "       three".to_owned()],
        );
    }

    #[test]
    fn the_replay_carries_the_whole_observation_and_the_frame_carries_the_first_line() {
        let line = Line::Work {
            command: "cat /etc/passwd".to_owned(),
            observation: "exit code 1\nstderr: denied\n".to_owned(),
        };

        let replayed = format!("{line}");
        assert!(
            replayed.contains("exit code 1") && replayed.contains("stderr: denied"),
            "the replay must carry every line of the observation: {replayed:?}",
        );

        assert_eq!(
            view(vec![line], 0).visible(usize::MAX, 2),
            vec![
                "  $ cat /etc/passwd".to_owned(),
                "    exit code 1".to_owned(),
            ],
            "the frame must keep the command row and the observation's first line",
        );
    }

    #[test]
    fn the_frame_and_the_replay_use_the_same_gutters() {
        for (line, gutter) in [
            (Line::Human("first\nsecond".to_owned()), "you    "),
            (Line::Agent("first\nsecond".to_owned()), "agent  "),
        ] {
            let drawn = view(vec![line.clone()], 0).visible(usize::MAX, 2);
            let replayed: Vec<String> = format!("{line}").lines().map(str::to_owned).collect();

            assert_eq!(drawn.len(), 2, "a two-line {line:?} must draw two rows");
            assert!(
                drawn[0].starts_with(gutter),
                "the drawn first row {drawn:?} must carry the gutter {gutter:?}",
            );
            assert!(
                replayed[0].starts_with(gutter),
                "the replayed first row {replayed:?} must carry the same gutter {gutter:?}",
            );
            assert!(
                drawn[1].starts_with("       "),
                "the drawn continuation row {drawn:?} must carry the seven-space prefix",
            );
            assert!(
                replayed[1].starts_with("       "),
                "the replayed continuation row {replayed:?} must carry the seven-space prefix",
            );
        }
    }

    #[test]
    fn a_trouble_row_carries_its_gutter_and_the_replay_carries_the_same_text() {
        let line = Line::Trouble(
            "the model did not answer in 3 attempts\nthe turn stopped at that".to_owned(),
        );

        let drawn = vec![
            "!      the model did not answer in 3 attempts".to_owned(),
            "       the turn stopped at that".to_owned(),
        ];

        assert_eq!(view(vec![line.clone()], 0).visible(usize::MAX, 2), drawn);
        assert_eq!(format!("{line}"), drawn.join("\n"));
    }

    #[test]
    fn a_carriage_return_before_a_line_break_is_one_break() {
        let crlf = view(vec![Line::Agent("one\r\ntwo".to_owned())], 0);

        assert_eq!(
            crlf.visible(usize::MAX, 2),
            vec!["agent  one".to_owned(), "       two".to_owned()],
            "a carriage return before a line break is part of the break, not a drawn character",
        );

        let lone = view(vec![Line::Agent("a\rb".to_owned())], 0);

        assert_eq!(
            lone.visible(usize::MAX, 1),
            vec!["agent  a\\rb".to_owned()],
            "a carriage return inside a line is still shown as the escape",
        );
    }

    #[test]
    fn a_control_character_that_is_not_a_break_is_drawn_as_a_question_mark() {
        let line = Line::Agent("bell\u{7}here".to_owned());

        assert_eq!(
            view(vec![line.clone()], 0).visible(usize::MAX, 1),
            vec!["agent  bell?here".to_owned()],
        );
        assert_eq!(format!("{line}"), "agent  bell?here");
    }

    /// Any text a line can hold, its line breaks and control characters among the choices.
    fn any_text() -> impl Strategy<Value = String> {
        let piece = prop_oneof![
            any::<char>(),
            Just('\n'),
            Just('\r'),
            Just('\t'),
            Just('\u{7}'),
        ];

        prop::collection::vec(piece, 0..12)
            .prop_map(|pieces| pieces.into_iter().collect::<String>())
    }

    proptest! {
        #[test]
        fn a_drawn_row_holds_no_control_character_and_matches_the_replay(text in any_text()) {
            let lines = [
                Line::Human(text.clone()),
                Line::Agent(text.clone()),
                Line::Trouble(text.clone()),
                Line::Work {
                    command: text.clone(),
                    observation: text.clone(),
                },
            ];

            for line in &lines {
                for row in view(vec![line.clone()], 0).visible(usize::MAX, usize::MAX) {
                    prop_assert!(
                        !row.chars().any(char::is_control),
                        "the drawn row {row:?} of {text:?} holds a control character",
                    );
                }
            }

            for line in &lines[..3] {
                let replayed: Vec<String> =
                    format!("{line}").split('\n').map(str::to_owned).collect();
                prop_assert_eq!(
                    view(vec![line.clone()], 0).visible(usize::MAX, replayed.len()),
                    replayed,
                    "the frame and the replay of {:?} differ", text,
                );
            }
        }
    }
}
