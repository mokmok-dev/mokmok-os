//! The ports: what the core needs from one session, from the log, and where a frame goes.

use std::future::Future;
use std::path::PathBuf;

use mokmok_agent_runtime::{Progress, Transcript};
use mokmok_protocol::SessionID;
use mokmok_state_machine::Phase;

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
    /// # Errors
    ///
    /// Returns the journal's, the provider's, or the tools' own failure.
    fn step(&mut self) -> impl Future<Output = Result<Progress, Error>> + Send;

    /// Re-opens the session, settling what a dead process left in flight.
    ///
    /// # Errors
    ///
    /// Returns the store's failure when the session cannot be re-opened.
    fn reopen(&mut self) -> Result<(), Error>;
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
    /// A turn the agent could not finish, and why.
    Trouble(String),
}

const HUMAN_GUTTER: &str = "you    ";

const AGENT_GUTTER: &str = "agent  ";

const TROUBLE_GUTTER: &str = "!      ";

// Seven columns, so a continuation row aligns under the first row's text.
const CONTINUATION_PREFIX: &str = "       ";

const COMMAND_PREFIX: &str = "  $ ";

const QUERY_PREFIX: &str = "  ? ";

const EDIT_PREFIX: &str = "  ✎ ";

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
                let mut rows = vec![command_row(command)];
                rows.extend(observation.lines().map(observation_row));
                rows
            },
            Self::Find { query, observation } => {
                let mut rows = vec![query_row(query)];
                rows.extend(observation.lines().map(observation_row));
                rows
            },
            Self::Edit { observation } => {
                let mut rows = vec![edit_row(observation)];
                rows.extend(observation.lines().skip(1).map(observation_row));
                rows
            },
            other => rows_of(other),
        };

        formatter.write_str(&rows.join("\n"))
    }
}

fn rows_of(line: &Line) -> Vec<String> {
    match line {
        Line::Human(text) => prose_rows(HUMAN_GUTTER, text),
        Line::Agent(text) => prose_rows(AGENT_GUTTER, text),
        Line::Trouble(text) => prose_rows(TROUBLE_GUTTER, text),
        Line::Work {
            command,
            observation,
        } => {
            let mut rows = vec![command_row(command)];

            // Four fixed rows is the frame's whole budget, so the first observation line is all it
            // can spare for the reason a command failed.
            if let Some(outcome) = observation.lines().next().filter(|first| !first.is_empty()) {
                rows.push(observation_row(outcome));
            }

            rows
        },
        Line::Find { query, observation } => {
            let mut rows = vec![query_row(query)];

            if let Some(outcome) = observation.lines().next().filter(|first| !first.is_empty()) {
                rows.push(observation_row(outcome));
            }

            rows
        },
        Line::Edit { observation } => {
            let mut rows = Vec::new();

            if let Some(first) = observation.lines().next().filter(|first| !first.is_empty()) {
                rows.push(edit_row(first));
            }
            if let Some(outcome) = observation
                .lines()
                .nth(1)
                .filter(|second| !second.is_empty())
            {
                rows.push(observation_row(outcome));
            }

            rows
        },
    }
}

fn command_row(command: &str) -> String {
    format!("{COMMAND_PREFIX}{}", escaped(command))
}

fn query_row(query: &str) -> String {
    format!("{QUERY_PREFIX}{}", escaped(query))
}

fn edit_row(observation: &str) -> String {
    format!("{EDIT_PREFIX}{}", escaped(observation))
}

fn observation_row(line: &str) -> String {
    format!("{OBSERVATION_PREFIX}{}", escaped(line))
}

/// One speaker's text as rows: the gutter leads the first line, seven spaces each later one.
///
/// A single trailing line break ends the last line rather than opening an empty one, so it draws
/// no blank row.
fn prose_rows(
    gutter: &str,
    text: &str,
) -> Vec<String> {
    let body = text.strip_suffix('\n').unwrap_or(text);

    body.split('\n')
        .enumerate()
        .map(|(index, line)| {
            let prefix = if index == 0 {
                gutter
            } else {
                CONTINUATION_PREFIX
            };
            // A carriage return before a line break is part of the break, not a character to draw.
            let line = line.strip_suffix('\r').unwrap_or(line);

            format!("{prefix}{}", escaped(line))
        })
        .collect()
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
    /// How many lines back from the newest the transcript is scrolled.
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

impl View {
    /// The rows a transcript `height` rows tall shows, newest last.
    ///
    /// A pure function of the view and the one thing the adapter knows, so which rows are on screen
    /// is decided here and tested without a terminal.
    ///
    /// The pane's width is not among the arguments, so a row wider than the pane is left to the
    /// renderer, which truncates it with no marker; measuring or wrapping a row would require the
    /// width in the signature.
    #[must_use]
    pub fn visible(
        &self,
        height: usize,
    ) -> Vec<String> {
        let rows: Vec<String> = self.lines.iter().flat_map(rows_of).collect();
        let end = rows.len().saturating_sub(usize::from(self.scroll));
        let start = end.saturating_sub(height);

        rows[start..end].to_vec()
    }

    /// The rows the body shows: the listing when the human opened it, and the transcript otherwise.
    ///
    /// Naming a session's goal leaves the conversation behind the pane, because the goal is usually a
    /// reading of what is already on screen.
    #[must_use]
    pub fn body(
        &self,
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
            _ => self.visible(height),
        }
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

    use super::{Line, Row, Sheet, View};
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
            view.body(10),
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
            view.body(1)[0].ends_with("(no goal recorded)"),
            "a row with no goal says there is none: {:?}",
            view.body(1),
        );
    }

    #[test]
    fn the_listing_holds_the_cursor_row_in_a_pane_shorter_than_the_rows() {
        let goals = [None; 5];
        let rows = |cursor| {
            listing(5, cursor, &goals)
                .body(3)
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
            listing(5, 0, &goals).body(5).len(),
            5,
            "a pane that fits shows every row"
        );
        assert!(
            listing(5, 0, &goals).body(0).is_empty(),
            "a pane with no room shows no row",
        );

        // An even pane height, where the rows kept above the cursor are not the rows between the top
        // of the window and the cursor.
        let even = listing(6, 3, &[None; 6]).body(4);

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
            naming.body(10),
            conversation.body(10),
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
            view.visible(10),
            vec![
                "you    hello".to_owned(),
                "  $ ls".to_owned(),
                "    out.txt".to_owned(),
                "agent  bye".to_owned(),
            ],
        );
        assert_eq!(view.visible(0), Vec::<String>::new());
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
            view.visible(10),
            vec![
                "  ? quokka".to_owned(),
                "    indexed=2 hits=1".to_owned(),
                "agent  found it".to_owned(),
            ],
            "a find must not draw the command's gutter",
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
            view(vec![line], 0).visible(2),
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
            view.visible(3),
            vec![
                "agent  fib.txt holds:".to_owned(),
                "       1, 1, 2, 3, 5".to_owned(),
                "       Anything else?".to_owned(),
            ],
            "each line of the reply is its own row, the later rows aligned under the gutter",
        );
        assert_eq!(
            view.visible(2),
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

        assert_eq!(one.visible(1), vec!["agent  one line".to_owned()]);

        let ended = view(vec![Line::Agent("one line\n".to_owned())], 0);

        assert_eq!(
            ended.visible(2),
            vec!["agent  one line".to_owned()],
            "the trailing line break ends the line instead of drawing a blank row after it",
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
            view.visible(3),
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
            view.visible(4),
            vec![
                "    out.txt".to_owned(),
                "agent  one".to_owned(),
                "       two".to_owned(),
                "       three".to_owned(),
            ],
            "a reply's continuation rows count toward a window shorter than the transcript",
        );
        assert_eq!(
            view.visible(2),
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
            view(vec![line], 0).visible(2),
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
            let drawn = view(vec![line.clone()], 0).visible(2);
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

        assert_eq!(view(vec![line.clone()], 0).visible(2), drawn);
        assert_eq!(format!("{line}"), drawn.join("\n"));
    }

    #[test]
    fn a_carriage_return_before_a_line_break_is_one_break() {
        let crlf = view(vec![Line::Agent("one\r\ntwo".to_owned())], 0);

        assert_eq!(
            crlf.visible(2),
            vec!["agent  one".to_owned(), "       two".to_owned()],
            "a carriage return before a line break is part of the break, not a drawn character",
        );

        let lone = view(vec![Line::Agent("a\rb".to_owned())], 0);

        assert_eq!(
            lone.visible(1),
            vec!["agent  a\\rb".to_owned()],
            "a carriage return inside a line is still shown as the escape",
        );
    }

    #[test]
    fn a_control_character_that_is_not_a_break_is_drawn_as_a_question_mark() {
        let line = Line::Agent("bell\u{7}here".to_owned());

        assert_eq!(
            view(vec![line.clone()], 0).visible(1),
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
                for row in view(vec![line.clone()], 0).visible(usize::MAX) {
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
                    view(vec![line.clone()], 0).visible(replayed.len()),
                    replayed,
                    "the frame and the replay of {:?} differ", text,
                );
            }
        }
    }
}
