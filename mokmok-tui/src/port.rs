//! The ports: what the core needs from one session, from the log, and where a frame goes.

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;

use mokmok_agent_runtime::{Progress, Transcript};
use mokmok_protocol::SessionID;
use mokmok_state_machine::Phase;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::Error;
use crate::markdown::Markdown;

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

const HUMAN_PREFIX: &str = "› ";

const AGENT_PREFIX: &str = "• ";

const TROUBLE_PREFIX: &str = "! ";

const WORK_PREFIX: &str = "  $ ";

const FIND_PREFIX: &str = "  ⌕ ";

const EDIT_PREFIX: &str = "  ✎ ";

const DELEGATE_PREFIX: &str = "  ⤷ ";

const CODE_PREFIX: &str = "  │ ";

const DETAIL_PREFIX: &str = "    ";

const CONTINUATION: &str = "  ";

/// What the input row starts with, so the line being typed and the goal being named are told apart.
const INPUT_PREFIX: &str = "› ";

const GOAL_PREFIX: &str = "goal› ";

impl std::fmt::Display for Line {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        let rows = replay(self);
        formatter.write_str(&rows.join("\n"))
    }
}

/// The line as plain rows, with every observation row the frame trims.
///
/// This is the record the human takes away after the alternate screen is handed back, so it keeps
/// the source text and drops only the markdown markers' styling, never their bytes.
fn replay(line: &Line) -> Vec<String> {
    match line {
        Line::Human(text) => prose_replay(HUMAN_PREFIX, text),
        Line::Agent(text) => prose_replay(AGENT_PREFIX, text),
        Line::Trouble(text) => prose_replay(TROUBLE_PREFIX, text),
        Line::Work {
            command,
            observation,
        } => {
            let mut rows = vec![format!("{WORK_PREFIX}{}", escaped(command))];
            rows.extend(
                observation
                    .lines()
                    .map(|line| format!("{DETAIL_PREFIX}{}", escaped(line))),
            );
            rows
        },
        Line::Find { query, observation } => {
            let mut rows = vec![format!("{FIND_PREFIX}{}", escaped(query))];
            rows.extend(
                observation
                    .lines()
                    .map(|line| format!("{DETAIL_PREFIX}{}", escaped(line))),
            );
            rows
        },
        Line::Edit { observation } => {
            let mut rows = Vec::new();
            let lines: Vec<&str> = observation
                .lines()
                .filter(|line| !line.is_empty())
                .collect();

            if let Some(first) = lines.first() {
                rows.push(format!("{EDIT_PREFIX}{}", escaped(first)));
            }
            rows.extend(
                lines
                    .iter()
                    .skip(1)
                    .map(|line| format!("{DETAIL_PREFIX}{}", escaped(line))),
            );
            rows
        },
        Line::Delegate {
            child_session,
            goal,
            reply,
        } => {
            let mut rows = vec![format!("{DELEGATE_PREFIX}delegate: {}", escaped(goal))];
            rows.push(format!("{DETAIL_PREFIX}child: {}", child_session.as_uuid()));
            rows.extend(
                reply
                    .lines()
                    .map(|line| format!("{DETAIL_PREFIX}{}", escaped(line))),
            );
            rows
        },
    }
}

fn prose_replay(
    prefix: &str,
    text: &str,
) -> Vec<String> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    let continuation = " ".repeat(prefix.width());

    body.split('\n')
        .enumerate()
        .map(|(index, line)| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let lead = if index == 0 {
                prefix.to_owned()
            } else {
                continuation.clone()
            };

            format!("{lead}{}", escaped(line))
        })
        .collect()
}

/// How a drawn span reads. The adapter maps a tone to a style and decides nothing else.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    /// What the human said.
    Human,
    /// What the agent said.
    Agent,
    /// A command or a finder's query.
    Command,
    /// What a command, a finder, or an edit produced.
    Observation,
    /// Code, inline or in a fenced block.
    Code,
    /// A heading or bold prose.
    Strong,
    /// A turn that could not finish.
    Trouble,
    /// Metadata: a prefix, a hint, a label.
    Muted,
    /// A marker that belongs to the human's own surface.
    Accent,
}

/// One piece of a drawn row, with the tone it reads in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Span {
    /// The text to place. It holds no control character.
    pub text: String,
    /// How it reads.
    pub tone: Tone,
}

impl Span {
    fn new(
        text: impl Into<String>,
        tone: Tone,
    ) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

/// One drawn row: the spans to place, and whether the listing's cursor is on it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    /// The spans, left to right.
    pub spans: Vec<Span>,
    /// Whether the listing's cursor is on this row, so the adapter may highlight it.
    pub selected: bool,
}

impl Row {
    /// The display width of the row, in terminal columns.
    #[must_use]
    pub fn width(&self) -> usize {
        self.spans.iter().map(|span| span.text.width()).sum()
    }

    /// The row's text with every span joined.
    #[must_use]
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }

    const fn plain(
        spans: Vec<Span>,
        selected: bool,
    ) -> Self {
        Self { spans, selected }
    }
}

/// One frame, as data: the renderer lays it out and decides nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct View {
    /// The session's name, so the human can read the conversation back with `mokmokd status`.
    pub session: SessionID,
    /// The goal the session's task recorded, when it recorded one.
    pub goal: Option<String>,
    /// The checkout the session's task recorded, when it recorded one.
    pub checkout: Option<PathBuf>,
    /// Every line of the conversation, oldest first.
    pub lines: Vec<Line>,
    /// The line being typed.
    pub input: String,
    /// One phrase for the session's state and whose turn it is.
    pub status: String,
    /// The key hints for the open pane, or the conversation's own.
    pub hints: String,
    /// The composer's placeholder, empty when there is nothing to type.
    pub placeholder: String,
    /// How many lines back from the newest the transcript is scrolled.
    pub scroll: u16,
    /// One phrase for the sessions whose turn it is besides the one on screen, when any are.
    ///
    /// `None` when nothing else is moving, so the frame of a single session is the frame it was.
    pub background: Option<String>,
    /// The pane that replaces the transcript, when the human opened one.
    pub sheet: Option<Sheet>,
    /// `Some(phase)` while the focused session's turn is moving, so the adapter draws a spinner.
    pub spinner: Option<u8>,
}

/// The line being typed, laid out for a pane `width` columns wide.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Typing {
    /// The rows to draw, the first behind the prompt and the rest indented past it.
    pub rows: Vec<Row>,
    /// Which row holds the cursor.
    pub row: usize,
    /// The column the next character takes.
    pub column: usize,
}

/// A pane the human opened over the conversation.
///
/// The cases are the two things he can be doing besides typing a line — choosing a session, and
/// naming one that does not exist yet — so the screen cannot invent a third.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Sheet {
    /// The sessions this process holds and the ones the log holds, with the cursor on one entry.
    List {
        /// The entries, in the order they are drawn.
        entries: Vec<Entry>,
        /// Which entry the cursor is on.
        cursor: usize,
    },
    /// The goal of a session that does not exist yet, as it is being typed.
    New {
        /// The goal so far.
        goal: String,
    },
}

/// One entry of the listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// The session the entry names.
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

const LIST_ID_WIDTH: usize = 8;

const LIST_STATUS_CAP: usize = 22;

const LIST_GAP: &str = "  ";

/// A logical row before wrapping: its first prefix, its continuation prefix, and its content.
struct Logical {
    prefix: Vec<Span>,
    continuation: Vec<Span>,
    content: Vec<Span>,
    preformatted: bool,
}

impl Logical {
    fn prefixed(
        prefix: &str,
        content: Vec<Span>,
    ) -> Self {
        Self {
            prefix: vec![Span::new(prefix, Tone::Muted)],
            continuation: vec![Span::new(" ".repeat(prefix.width()), Tone::Muted)],
            content,
            preformatted: false,
        }
    }

    fn detail(content: Vec<Span>) -> Self {
        Self::prefixed(DETAIL_PREFIX, content)
    }

    const fn plain(content: Vec<Span>) -> Self {
        Self {
            prefix: Vec::new(),
            continuation: Vec::new(),
            content,
            preformatted: false,
        }
    }

    fn field(
        label: &str,
        value: &str,
        tone: Tone,
    ) -> Self {
        let prefix = format!("{label:<10}");

        Self {
            prefix: vec![Span::new(prefix.clone(), Tone::Muted)],
            continuation: vec![Span::new(" ".repeat(prefix.width()), Tone::Muted)],
            content: vec![Span::new(escaped(value), tone)],
            preformatted: false,
        }
    }
}

fn prose_lines(
    prefix: &str,
    prefix_tone: Tone,
    tone: Tone,
    text: &str,
) -> Vec<Logical> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    let continuation = " ".repeat(prefix.width());

    body.split('\n')
        .enumerate()
        .map(|(index, line)| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let held = if index == 0 {
                prefix
            } else {
                continuation.as_str()
            };

            Logical {
                prefix: vec![Span::new(
                    held,
                    if index == 0 { prefix_tone } else { Tone::Muted },
                )],
                continuation: vec![Span::new(continuation.clone(), Tone::Muted)],
                content: vec![Span::new(escaped(line), tone)],
                preformatted: false,
            }
        })
        .collect()
}

fn agent_lines(text: &str) -> Vec<Logical> {
    let mut markdown = Markdown::new();
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut rows = Vec::new();
    let mut first = true;

    for line in body.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let Some(prose) = markdown.line(line) else {
            continue;
        };
        let content: Vec<Span> = prose
            .spans
            .into_iter()
            .map(|(text, tone)| Span::new(text, tone))
            .collect();

        if prose.code {
            rows.push(Logical {
                prefix: vec![Span::new(CODE_PREFIX, Tone::Muted)],
                continuation: vec![Span::new(CODE_PREFIX, Tone::Muted)],
                content,
                preformatted: true,
            });
        } else {
            let held = if first { AGENT_PREFIX } else { CONTINUATION };
            first = false;
            rows.push(Logical {
                prefix: vec![Span::new(held, Tone::Muted)],
                continuation: vec![Span::new(CONTINUATION, Tone::Muted)],
                content,
                preformatted: false,
            });
        }
    }

    rows
}

fn work_lines(
    command: &str,
    observation: &str,
) -> Vec<Logical> {
    let mut rows = vec![Logical::prefixed(
        WORK_PREFIX,
        vec![Span::new(escaped(command), Tone::Command)],
    )];

    if let Some(first) = observation.lines().next().filter(|line| !line.is_empty()) {
        rows.push(Logical::detail(vec![Span::new(
            escaped(first.strip_suffix('\r').unwrap_or(first)),
            Tone::Observation,
        )]));
    }

    rows
}

fn find_lines(
    query: &str,
    observation: &str,
) -> Vec<Logical> {
    let mut rows = vec![Logical::prefixed(
        FIND_PREFIX,
        vec![Span::new(escaped(query), Tone::Command)],
    )];

    if let Some(first) = observation.lines().next().filter(|line| !line.is_empty()) {
        rows.push(Logical::detail(vec![Span::new(
            escaped(first.strip_suffix('\r').unwrap_or(first)),
            Tone::Observation,
        )]));
    }

    rows
}

fn edit_lines(observation: &str) -> Vec<Logical> {
    let lines: Vec<&str> = observation
        .lines()
        .filter(|line| !line.is_empty())
        .collect();
    let mut rows = Vec::new();

    if let Some(first) = lines.first() {
        rows.push(Logical::prefixed(
            EDIT_PREFIX,
            vec![Span::new(escaped(first), Tone::Command)],
        ));
    }
    if let Some(second) = lines.get(1) {
        rows.push(Logical::detail(vec![Span::new(
            escaped(second),
            Tone::Observation,
        )]));
    }

    rows
}

fn delegate_lines(
    child_session: SessionID,
    goal: &str,
    reply: &str,
) -> Vec<Logical> {
    let mut rows = vec![Logical::prefixed(
        DELEGATE_PREFIX,
        vec![
            Span::new("delegate: ", Tone::Muted),
            Span::new(escaped(goal), Tone::Strong),
        ],
    )];
    rows.push(Logical::detail(vec![
        Span::new("child: ", Tone::Muted),
        Span::new(child_session.as_uuid().to_string(), Tone::Observation),
    ]));

    if let Some(first) = reply.lines().next().filter(|line| !line.is_empty()) {
        rows.push(Logical::detail(vec![Span::new(
            escaped(first),
            Tone::Observation,
        )]));
    }

    rows
}

fn line_rows(line: &Line) -> Vec<Logical> {
    match line {
        Line::Human(text) => prose_lines(HUMAN_PREFIX, Tone::Accent, Tone::Human, text),
        Line::Agent(text) => agent_lines(text),
        Line::Work {
            command,
            observation,
        } => work_lines(command, observation),
        Line::Find { query, observation } => find_lines(query, observation),
        Line::Edit { observation } => edit_lines(observation),
        Line::Delegate {
            child_session,
            goal,
            reply,
        } => delegate_lines(*child_session, goal, reply),
        Line::Trouble(text) => prose_lines(TROUBLE_PREFIX, Tone::Trouble, Tone::Trouble, text),
    }
}

/// Wraps every logical row and turns it into drawn rows.
fn draw(
    logicals: Vec<Logical>,
    width: usize,
) -> Vec<Row> {
    if width == 0 {
        return Vec::new();
    }

    let mut rows = Vec::new();

    for logical in logicals {
        let prefix_width: usize = logical.prefix.iter().map(|span| span.text.width()).sum();
        let available = width.saturating_sub(prefix_width).max(1);
        let chunks = if logical.preformatted {
            wrap_preformatted(&logical.content, available)
        } else {
            wrap_spans(&logical.content, available)
        };

        if chunks.is_empty() {
            rows.push(Row::plain(logical.prefix, false));
            continue;
        }

        for (index, chunk) in chunks.into_iter().enumerate() {
            let mut spans = if index == 0 {
                logical.prefix.clone()
            } else {
                logical.continuation.clone()
            };
            spans.extend(chunk);
            rows.push(Row::plain(spans, false));
        }
    }

    rows
}

struct Token {
    text: String,
    tone: Tone,
    space: bool,
}

fn tokens(spans: &[Span]) -> Vec<Token> {
    let mut tokens = Vec::new();

    for span in spans {
        let mut word = String::new();

        for character in span.text.chars() {
            if character == ' ' {
                if !word.is_empty() {
                    tokens.push(Token {
                        text: std::mem::take(&mut word),
                        tone: span.tone,
                        space: false,
                    });
                }
                tokens.push(Token {
                    text: " ".to_owned(),
                    tone: span.tone,
                    space: true,
                });
            } else {
                word.push(character);
            }
        }

        if !word.is_empty() {
            tokens.push(Token {
                text: word,
                tone: span.tone,
                space: false,
            });
        }
    }

    tokens
}

fn wrap_spans(
    spans: &[Span],
    width: usize,
) -> Vec<Vec<Span>> {
    let mut tokens: VecDeque<Token> = tokens(spans).into();
    let mut rows = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    let mut used = 0;

    while let Some(token) = tokens.pop_front() {
        if token.space {
            if used == 0 {
                continue;
            }

            let Some(next) = tokens.iter().find(|token| !token.space) else {
                continue;
            };

            if used + 1 + next.text.width() <= width {
                push_text(&mut current, " ", token.tone);
                used += 1;
            } else {
                rows.push(std::mem::take(&mut current));
                used = 0;
                while tokens.front().is_some_and(|token| token.space) {
                    tokens.pop_front();
                }
            }
            continue;
        }

        let word_width = token.text.width();

        if used + word_width <= width {
            push_text(&mut current, &token.text, token.tone);
            used += word_width;
        } else {
            if !current.is_empty() {
                rows.push(std::mem::take(&mut current));
                used = 0;
            }

            if word_width <= width {
                push_text(&mut current, &token.text, token.tone);
                used = word_width;
            } else {
                let mut rest = token.text.as_str();

                while !rest.is_empty() {
                    let cut = char_boundary(rest, width);
                    if cut == 0 {
                        break;
                    }
                    let (head, tail) = rest.split_at(cut);

                    if tail.is_empty() {
                        push_text(&mut current, head, token.tone);
                        used = head.width();
                        break;
                    }

                    rows.push(vec![Span::new(head, token.tone)]);
                    rest = tail;
                }
            }
        }
    }

    if !current.is_empty() {
        rows.push(current);
    }

    rows
}

fn wrap_preformatted(
    spans: &[Span],
    width: usize,
) -> Vec<Vec<Span>> {
    let mut rows = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    let mut used = 0;

    for span in spans {
        let mut rest = span.text.as_str();

        while !rest.is_empty() {
            let cut = char_boundary(rest, width.saturating_sub(used).max(1));
            if cut == 0 {
                break;
            }
            let (head, tail) = rest.split_at(cut);
            push_text(&mut current, head, span.tone);
            used += head.width();
            rest = tail;

            if used >= width {
                rows.push(std::mem::take(&mut current));
                used = 0;
            }
        }
    }

    if !current.is_empty() {
        rows.push(current);
    }

    rows
}

/// The byte index after the longest prefix whose display width is at most `width`.
///
/// A single character wider than `width` is returned whole, so a caller always makes progress.
fn char_boundary(
    text: &str,
    width: usize,
) -> usize {
    let mut used = 0;
    let mut end = 0;

    for (index, character) in text.char_indices() {
        let character_width = character.width().unwrap_or(0);

        if used + character_width > width {
            if index == 0 {
                return character.len_utf8();
            }
            break;
        }

        used += character_width;
        end = index + character.len_utf8();
    }

    end
}

fn push_text(
    spans: &mut Vec<Span>,
    text: &str,
    tone: Tone,
) {
    if text.is_empty() {
        return;
    }

    if let Some(last) = spans.last_mut()
        && last.tone == tone
    {
        last.text.push_str(text);

        return;
    }

    spans.push(Span::new(text, tone));
}

fn short_id(session: SessionID) -> String {
    session
        .as_uuid()
        .to_string()
        .chars()
        .take(LIST_ID_WIDTH)
        .collect()
}

/// Clips `text` to `width` columns, ending with an ellipsis when it does not fit.
#[must_use]
pub fn clipped(
    text: &str,
    width: usize,
) -> String {
    let text = escaped(text);

    if text.width() <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_owned();
    }

    let mut clipped = String::new();
    let mut used = 0;

    for character in text.chars() {
        let character_width = character.width().unwrap_or(0);

        if used + character_width > width - 1 {
            break;
        }

        clipped.push(character);
        used += character_width;
    }

    clipped.push('…');

    clipped
}

fn window(
    rows: &[Row],
    cursor: usize,
    height: usize,
) -> Vec<Row> {
    let start = cursor
        .saturating_sub(height / 2)
        .min(rows.len().saturating_sub(height));

    rows[start..].iter().take(height).cloned().collect()
}

impl View {
    /// The first eight characters of the session's identity, which is what a row can spare.
    #[must_use]
    pub fn short_session(&self) -> String {
        short_id(self.session)
    }

    /// The rows a transcript `height` rows tall shows, newest last, wrapped to `width` columns.
    ///
    /// A pure function of the view and the two things the adapter knows, so which rows are on
    /// screen is decided here and tested without a terminal.
    #[must_use]
    pub fn visible(
        &self,
        width: usize,
        height: usize,
    ) -> Vec<Row> {
        let rows: Vec<Row> = self
            .lines
            .iter()
            .flat_map(|line| draw(line_rows(line), width))
            .collect();
        let end = rows.len().saturating_sub(usize::from(self.scroll));
        let start = end.saturating_sub(height);

        rows[start..end].to_vec()
    }

    /// The rows the body shows: the listing when the human opened it, and the transcript otherwise.
    ///
    /// Naming a session's goal leaves the conversation behind the pane, because the goal is usually
    /// a reading of what is already on screen.
    #[must_use]
    pub fn body(
        &self,
        width: usize,
        height: usize,
    ) -> Vec<Row> {
        match &self.sheet {
            Some(Sheet::List { entries, cursor }) => Self::listing(entries, *cursor, width, height),
            _ if self.lines.is_empty() => self.welcome(width, height),
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
        let prompt = match &self.sheet {
            Some(Sheet::List { .. }) => {
                return Typing {
                    rows: vec![Row::plain(Vec::new(), false)],
                    row: 0,
                    column: 0,
                };
            },
            Some(Sheet::New { .. }) => GOAL_PREFIX,
            None => INPUT_PREFIX,
        };
        let prefix_width = prompt.width();
        let available = width.saturating_sub(prefix_width).max(1);
        let text = match &self.sheet {
            Some(Sheet::New { goal }) => goal.as_str(),
            Some(Sheet::List { .. }) => "",
            None => self.input.as_str(),
        };

        if text.is_empty() {
            let mut spans = vec![Span::new(prompt, Tone::Accent)];

            if !self.placeholder.is_empty() {
                spans.push(Span::new(
                    clipped(&self.placeholder, available),
                    Tone::Muted,
                ));
            }

            return Typing {
                rows: vec![Row::plain(spans, false)],
                row: 0,
                column: prefix_width.min(width.saturating_sub(1)),
            };
        }

        let content = vec![Span::new(escaped(text), Tone::Agent)];
        let chunks = wrap_spans(&content, available);
        let mut rows = Vec::new();

        for (index, chunk) in chunks.into_iter().enumerate() {
            let mut spans = if index == 0 {
                vec![Span::new(prompt, Tone::Accent)]
            } else {
                vec![Span::new(" ".repeat(prefix_width), Tone::Muted)]
            };
            spans.extend(chunk);
            rows.push(Row::plain(spans, false));
        }

        let mut row = rows.len() - 1;
        let mut column = rows[row].width();

        if column >= width {
            rows.push(Row::plain(
                vec![Span::new(" ".repeat(prefix_width), Tone::Muted)],
                false,
            ));
            row += 1;
            column = prefix_width;
        }

        Typing {
            rows,
            row,
            column: column.min(width.saturating_sub(1)),
        }
    }

    fn welcome(
        &self,
        width: usize,
        height: usize,
    ) -> Vec<Row> {
        let mut logicals = vec![
            Logical::plain(vec![Span::new(
                "A conversation with the constrained agent. Type a line and press enter.",
                Tone::Muted,
            )]),
            Logical::plain(Vec::new()),
        ];

        if let Some(goal) = &self.goal {
            logicals.push(Logical::field("goal", goal, Tone::Agent));
        }
        if let Some(checkout) = &self.checkout {
            logicals.push(Logical::field(
                "checkout",
                &checkout.display().to_string(),
                Tone::Observation,
            ));
        }

        logicals.push(Logical::plain(Vec::new()));
        logicals.push(Logical::plain(vec![Span::new(
            "^O lists every session this log holds.",
            Tone::Muted,
        )]));

        draw(logicals, width).into_iter().take(height).collect()
    }

    fn listing(
        entries: &[Entry],
        cursor: usize,
        width: usize,
        height: usize,
    ) -> Vec<Row> {
        let mut rows = Vec::new();

        // A header is worth a row only when the pane can spare one beside a list worth scanning.
        if height >= 4 {
            rows.push(Row::plain(vec![Span::new("sessions", Tone::Muted)], false));
        }

        let room = height - rows.len();

        if entries.is_empty() {
            rows.push(Row::plain(
                vec![Span::new(
                    "no session is written in this log yet",
                    Tone::Muted,
                )],
                false,
            ));

            return rows.into_iter().take(height).collect();
        }

        let status_width = entries
            .iter()
            .map(|entry| clipped(&entry.status, LIST_STATUS_CAP).width())
            .max()
            .unwrap_or(0);
        let drawn: Vec<Row> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| Self::entry_row(entry, index == cursor, status_width, width))
            .collect();

        rows.extend(window(&drawn, cursor, room));

        rows
    }

    fn entry_row(
        entry: &Entry,
        selected: bool,
        status_width: usize,
        width: usize,
    ) -> Row {
        let mut spans = Vec::new();

        push_text(&mut spans, if selected { "› " } else { "  " }, Tone::Accent);
        push_text(
            &mut spans,
            if entry.held { "* " } else { "  " },
            Tone::Command,
        );
        push_text(&mut spans, &short_id(entry.session), Tone::Observation);
        push_text(&mut spans, LIST_GAP, Tone::Muted);

        let status = clipped(&entry.status, status_width);
        let padding = status_width.saturating_sub(status.width());
        push_text(&mut spans, &status, Tone::Agent);
        push_text(&mut spans, &" ".repeat(padding), Tone::Muted);
        push_text(&mut spans, LIST_GAP, Tone::Muted);

        let used: usize = spans.iter().map(|span| span.text.width()).sum();
        let room = width.saturating_sub(used);
        let goal = entry.goal.as_deref().unwrap_or(NO_GOAL);

        push_text(
            &mut spans,
            &clipped(goal, room),
            if entry.goal.is_some() {
                Tone::Agent
            } else {
                Tone::Muted
            },
        );

        Row::plain(spans, selected)
    }
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

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use uuid::Uuid;

    use super::{Entry, Line, Row, Sheet, Tone, Typing, View};
    use mokmok_protocol::SessionID;

    fn view(
        lines: Vec<Line>,
        scroll: u16,
    ) -> View {
        View {
            session: SessionID::from_uuid(Uuid::from_u128(1)),
            goal: None,
            checkout: None,
            lines,
            input: String::new(),
            status: String::new(),
            hints: String::new(),
            placeholder: String::new(),
            scroll,
            background: None,
            sheet: None,
            spinner: None,
        }
    }

    fn named(seed: u128) -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(seed << 120))
    }

    fn entry(
        session: SessionID,
        goal: Option<&str>,
        held: bool,
    ) -> Entry {
        Entry {
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
        let entries = (0..count)
            .map(|index| {
                entry(
                    named(u128::try_from(index).unwrap() + 1),
                    goals.get(index).copied().flatten(),
                    index == 0,
                )
            })
            .collect();

        View {
            sheet: Some(Sheet::List { entries, cursor }),
            ..view(Vec::new(), 0)
        }
    }

    fn texts(rows: &[Row]) -> Vec<String> {
        rows.iter().map(Row::text).collect()
    }

    fn content(rows: &[Row]) -> String {
        rows.iter()
            .map(|row| row.text().chars().skip(2).collect::<String>())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_human_and_an_agent_turn_carry_their_own_markers() {
        let view = view(
            vec![
                Line::Human("hello".to_owned()),
                Line::Agent("bye".to_owned()),
            ],
            0,
        );

        assert_eq!(texts(&view.visible(20, 10)), vec!["› hello", "• bye"]);
    }

    #[test]
    fn a_work_row_shows_the_command_and_its_first_outcome() {
        let view = view(
            vec![Line::Work {
                command: "ls".to_owned(),
                observation: "out.txt\nmore\n".to_owned(),
            }],
            0,
        );
        let rows = view.visible(30, 10);

        assert_eq!(texts(&rows), vec!["  $ ls", "    out.txt"]);
        assert_eq!(rows[0].spans[1].tone, Tone::Command);
        assert_eq!(rows[1].spans[1].tone, Tone::Observation);
    }

    #[test]
    fn a_work_row_with_no_outcome_is_the_command_alone() {
        let view = view(
            vec![Line::Work {
                command: "ls".to_owned(),
                observation: String::new(),
            }],
            0,
        );

        assert_eq!(texts(&view.visible(30, 10)), vec!["  $ ls"]);
    }

    #[test]
    fn a_find_row_is_not_a_command() {
        let view = view(
            vec![Line::Find {
                query: "quokka".to_owned(),
                observation: "indexed=2 hits=1\n1.500  notes.txt\n".to_owned(),
            }],
            0,
        );

        assert_eq!(
            texts(&view.visible(40, 10)),
            vec!["  ⌕ quokka", "    indexed=2 hits=1"],
        );
    }

    #[test]
    fn an_edit_row_shows_its_first_two_lines() {
        let view = view(
            vec![Line::Edit {
                observation: "edit applied to src/x.rs\n1 bytes became 1 bytes\n".to_owned(),
            }],
            0,
        );

        assert_eq!(
            texts(&view.visible(50, 10)),
            vec!["  ✎ edit applied to src/x.rs", "    1 bytes became 1 bytes",],
        );
    }

    #[test]
    fn an_edit_skips_a_blank_line_between_its_rows() {
        let line = Line::Edit {
            observation: "first\n\nsecond\n".to_owned(),
        };
        let view = view(vec![line.clone()], 0);

        assert_eq!(
            texts(&view.visible(40, 10)),
            vec!["  ✎ first", "    second"],
        );
        assert_eq!(format!("{line}"), "  ✎ first\n    second");
    }

    #[test]
    fn a_delegate_row_shows_the_goal_the_child_and_the_reply() {
        let session = named(2);
        let view = view(
            vec![Line::Delegate {
                child_session: session,
                goal: "find the license".to_owned(),
                reply: "MIT license\nsummary here".to_owned(),
            }],
            0,
        );

        assert_eq!(
            texts(&view.visible(60, 10)),
            vec![
                "  ⤷ delegate: find the license".to_owned(),
                format!("    child: {}", session.as_uuid()),
                "    MIT license".to_owned(),
            ],
        );
    }

    #[test]
    fn a_trouble_row_carries_its_reason() {
        let view = view(
            vec![Line::Trouble(
                "the model did not answer\nno attempt was usable".to_owned(),
            )],
            0,
        );

        assert_eq!(
            texts(&view.visible(50, 10)),
            vec!["! the model did not answer", "  no attempt was usable",],
        );
    }

    #[test]
    fn a_reply_wraps_with_a_hanging_indent_under_its_marker() {
        let view = view(
            vec![Line::Agent("one two three four five six seven".to_owned())],
            0,
        );
        let rows = view.visible(16, 10);

        assert!(rows.len() > 2, "the reply wraps: {rows:?}");
        assert_eq!(rows[0].text().chars().next(), Some('•'));

        for row in rows.iter().skip(1) {
            assert!(
                row.text().starts_with("  "),
                "a continuation starts under the marker: {row:?}",
            );
        }
        for row in &rows {
            assert!(
                row.width() <= 16,
                "a wrapped row fits the width: {:?}",
                row.text(),
            );
        }
        assert_eq!(
            content(&rows),
            "one two three four five six seven",
            "wrapping drops no word and no space between words",
        );
    }

    #[test]
    fn a_word_wider_than_the_line_is_split() {
        let view = view(vec![Line::Agent("abcdefghij".to_owned())], 0);

        assert_eq!(
            texts(&view.visible(6, 10)),
            vec!["• abcd", "  efgh", "  ij"],
        );
    }

    #[test]
    fn a_wide_character_is_counted_by_columns() {
        let view = view(vec![Line::Agent("日本語".to_owned())], 0);

        assert_eq!(texts(&view.visible(8, 10)), vec!["• 日本語"]);
        assert_eq!(
            texts(&view.visible(7, 10)),
            vec!["• 日本", "  語"],
            "a wide character moves as one column pair",
        );
    }

    #[test]
    fn a_code_fence_draws_with_a_bar_and_no_markers() {
        let view = view(
            vec![Line::Agent(
                "# Title\n```\nlet x = 1;\n```\nplain".to_owned(),
            )],
            0,
        );
        let rows = view.visible(40, 20);

        assert_eq!(texts(&rows), vec!["• Title", "  │ let x = 1;", "  plain"],);
        assert_eq!(rows[0].spans[1].tone, Tone::Strong);
        assert_eq!(rows[1].spans[1].tone, Tone::Code);
    }

    #[test]
    fn a_code_line_wider_than_the_pane_is_hard_wrapped() {
        let view = view(vec![Line::Agent("```\nabcdefghij\n```".to_owned())], 0);

        assert_eq!(
            texts(&view.visible(6, 10)),
            vec!["  │ ab", "  │ cd", "  │ ef", "  │ gh", "  │ ij"],
        );
    }

    #[test]
    fn an_inline_marker_produces_its_own_tone() {
        let view = view(vec![Line::Agent("use `x` now".to_owned())], 0);
        let rows = view.visible(30, 10);

        assert_eq!(rows[0].text(), "• use x now");
        assert_eq!(
            rows[0]
                .spans
                .iter()
                .map(|span| span.tone)
                .collect::<Vec<_>>(),
            vec![Tone::Muted, Tone::Agent, Tone::Code, Tone::Agent],
        );
    }

    #[test]
    fn scrolling_counts_wrapped_rows() {
        let base = view(vec![Line::Agent("one two three four five".to_owned())], 0);
        let all = base.visible(12, 10);

        assert!(all.len() >= 2, "the reply wraps: {all:?}");

        let one_back = View { scroll: 1, ..base }.visible(12, all.len() - 1);
        assert_eq!(
            one_back.last().map(Row::text),
            all.get(all.len() - 2).map(Row::text),
            "one row back drops the newest wrapped row",
        );
    }

    #[test]
    fn the_newest_rows_survive_a_transcript_taller_than_the_pane() {
        let view = view(
            vec![
                Line::Human("first".to_owned()),
                Line::Agent("second".to_owned()),
                Line::Human("third".to_owned()),
                Line::Agent("fourth".to_owned()),
            ],
            0,
        );

        assert_eq!(texts(&view.visible(20, 2)), vec!["› third", "• fourth"],);
    }

    #[test]
    fn a_zero_width_draws_nothing() {
        let view = view(vec![Line::Human("hello".to_owned())], 0);

        assert_eq!(view.visible(0, 10), Vec::new());
        assert_eq!(view.visible(10, 0), Vec::new());
    }

    #[test]
    fn the_listing_draws_the_cursor_the_held_mark_and_the_goal() {
        let view = listing(
            2,
            0,
            &[Some("hold a conversation"), Some("count the quokkas")],
        );

        assert_eq!(
            texts(&view.body(60, 10)),
            vec![
                "sessions",
                "› * 01000000  your turn  hold a conversation",
                "    02000000  your turn  count the quokkas",
            ],
            "the cursor row, the held row, and each goal are drawn",
        );
        assert!(view.body(60, 10)[1].selected);
    }

    #[test]
    fn a_session_that_recorded_no_goal_says_so_in_its_entry() {
        let view = listing(1, 0, &[None]);

        assert!(
            view.body(60, 10)[1].text().ends_with("(no goal recorded)"),
            "an entry with no goal says there is none: {:?}",
            texts(&view.body(60, 10)),
        );
    }

    #[test]
    fn the_listing_holds_the_cursor_entry_in_a_pane_shorter_than_the_entries() {
        let goals = [None; 5];
        let rows = |cursor| {
            listing(5, cursor, &goals)
                .body(30, 3)
                .iter()
                .map(|row| row.text().chars().next().unwrap())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            rows(0),
            vec!['›', ' ', ' '],
            "the cursor at the top stays there"
        );
        assert_eq!(
            rows(4),
            vec![' ', ' ', '›'],
            "the cursor at the bottom stays there",
        );
        assert_eq!(
            rows(2),
            vec![' ', '›', ' '],
            "the cursor in the middle is centred"
        );
        assert_eq!(
            listing(5, 0, &goals).body(30, 5).len(),
            5,
            "a pane that fits shows every entry",
        );
    }

    #[test]
    fn a_two_row_listing_keeps_the_cursor_on_the_second_row() {
        let goals = [None; 5];
        let rows = listing(5, 2, &goals).body(30, 2);

        assert_eq!(rows.len(), 2);
        assert!(rows[0].text().starts_with(' '));
        assert!(rows[1].text().starts_with('›'));
    }

    #[test]
    fn an_empty_listing_says_so() {
        let view = listing(0, 0, &[]);

        assert_eq!(
            texts(&view.body(40, 5)),
            vec!["sessions", "no session is written in this log yet"],
        );
    }

    #[test]
    fn the_welcome_names_the_goal_and_the_checkout() {
        let view = View {
            goal: Some("hold a conversation".to_owned()),
            checkout: Some("/checkout".into()),
            ..view(Vec::new(), 0)
        };
        let shown = texts(&view.body(60, 10)).join("\n");

        assert!(shown.contains("A conversation with the constrained agent"));
        assert!(shown.contains("goal"), "the welcome labels the goal");
        assert!(shown.contains("hold a conversation"));
        assert!(shown.contains("checkout"));
        assert!(shown.contains("/checkout"));
    }

    #[test]
    fn the_welcome_keeps_only_the_rows_that_fit() {
        let view = View {
            goal: Some("hold a conversation".to_owned()),
            ..view(Vec::new(), 0)
        };

        assert_eq!(view.body(60, 2).len(), 2);
        assert_eq!(view.body(60, 0), Vec::new());
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
            naming.body(40, 10),
            conversation.body(40, 10),
            "the pane that names a goal draws the conversation it is about",
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
                rows: vec![
                    Row::plain(
                        vec![
                            super::Span::new("› ", Tone::Accent),
                            super::Span::new("abcdefgh", Tone::Agent)
                        ],
                        false
                    ),
                    Row::plain(
                        vec![
                            super::Span::new("  ", Tone::Muted),
                            super::Span::new("ij", Tone::Agent)
                        ],
                        false
                    ),
                ],
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
        let typing = view.typing(10);

        assert_eq!(texts(&typing.rows), vec!["› abcdefgh", "  "]);
        assert_eq!((typing.row, typing.column), (1, 2));
    }

    #[test]
    fn typing_counts_a_wide_character_as_two_columns() {
        let wide = View {
            input: "日本語".to_owned(),
            ..view(Vec::new(), 0)
        };
        let typing = wide.typing(80);

        assert_eq!(texts(&typing.rows), vec!["› 日本語"]);
        assert_eq!((typing.row, typing.column), (0, 8));

        let empty = view(Vec::new(), 0).typing(80);

        assert_eq!((empty.row, empty.column), (0, 2));
    }

    #[test]
    fn typing_a_goal_wraps_and_places_the_cursor_past_its_own_prompt() {
        let naming = View {
            sheet: Some(Sheet::New {
                goal: "count the quokkas".to_owned(),
            }),
            ..view(Vec::new(), 0)
        };
        let wide = naming.typing(80);

        assert_eq!(texts(&wide.rows), vec!["goal› count the quokkas"]);
        assert_eq!((wide.row, wide.column), (0, 23));

        let narrow = naming.typing(14);

        assert_eq!(
            texts(&narrow.rows),
            vec!["goal› count", "      the", "      quokkas"],
        );
        assert_eq!((narrow.row, narrow.column), (2, 13));
    }

    #[test]
    fn a_pane_narrower_than_its_prompt_keeps_the_cursor_on_its_last_column() {
        let naming = View {
            sheet: Some(Sheet::New {
                goal: "abc".to_owned(),
            }),
            ..view(Vec::new(), 0)
        };
        let typing = naming.typing(1);

        assert_eq!(
            texts(&typing.rows),
            vec!["goal› a", "      b", "      c", "      "],
        );
        assert_eq!((typing.row, typing.column), (3, 0));
    }

    #[test]
    fn the_listing_leaves_the_input_row_blank() {
        let view = listing(1, 0, &[None]);

        assert_eq!(
            view.typing(80),
            Typing {
                rows: vec![Row::plain(Vec::new(), false)],
                row: 0,
                column: 0,
            },
        );
    }

    #[test]
    fn an_empty_input_leaves_the_cursor_past_the_prompt_and_shows_the_placeholder() {
        let view = View {
            placeholder: "Send a line to the agent…".to_owned(),
            ..view(Vec::new(), 0)
        };
        let typing = view.typing(80);

        assert_eq!(texts(&typing.rows), vec!["› Send a line to the agent…"]);
        assert_eq!((typing.row, typing.column), (0, 2));
    }

    #[test]
    fn the_replay_carries_every_observation_row() {
        let line = Line::Work {
            command: "cat /etc/passwd".to_owned(),
            observation: "exit code 1\nstderr: denied\n".to_owned(),
        };
        let replayed = format!("{line}");

        assert_eq!(
            replayed,
            "  $ cat /etc/passwd\n    exit code 1\n    stderr: denied",
        );
    }

    #[test]
    fn the_replay_carries_every_reply_row() {
        let line = Line::Agent("# Title\nbody".to_owned());
        let replayed = format!("{line}");

        assert_eq!(replayed, "• # Title\n  body");
    }

    #[test]
    fn a_carriage_return_before_a_line_break_is_one_break() {
        let view = view(vec![Line::Agent("one\r\ntwo".to_owned())], 0);

        assert_eq!(texts(&view.visible(20, 10)), vec!["• one", "  two"]);
    }

    #[test]
    fn a_control_character_that_is_not_a_break_is_drawn_as_a_question_mark() {
        let view = view(vec![Line::Agent("bell\u{7}here".to_owned())], 0);

        assert_eq!(texts(&view.visible(20, 10)), vec!["• bell?here"]);
        assert_eq!(
            format!("{}", Line::Agent("bell\u{7}here".to_owned())),
            "• bell?here"
        );
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
        fn a_drawn_row_holds_no_control_character_and_fits_the_width(
            text in any_text(),
            width in 7usize..40,
        ) {
            let lines = [
                Line::Human(text.clone()),
                Line::Agent(text.clone()),
                Line::Trouble(text.clone()),
                Line::Work {
                    command: text.clone(),
                    observation: text.clone(),
                },
                Line::Find {
                    query: text.clone(),
                    observation: text.clone(),
                },
            ];

            for line in &lines {
                for row in view(vec![line.clone()], 0).visible(width, usize::MAX) {
                    prop_assert!(
                        !row.text().chars().any(char::is_control),
                        "the drawn row {:?} of {:?} holds a control character",
                        row.text(),
                        text,
                    );
                    prop_assert!(
                        row.width() <= width,
                        "the drawn row {:?} of {:?} is wider than {width}",
                        row.text(),
                        text,
                    );
                }
            }
        }
    }
}
