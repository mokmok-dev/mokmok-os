//! The ratatui adapter: crossterm events in, one frame out.

use std::io::IsTerminal;
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Position};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span as TextSpan};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal as RtTerminal};
use unicode_width::UnicodeWidthStr;

use crate::Error;
use crate::port::{Event, Key, Row, Screen, Span, Tone, View};

/// The frames of the braille spinner, in the order a working turn walks them.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

const RULE: char = '─';

/// The real terminal: keys from crossterm, one frame to the alternate screen.
pub struct Terminal {
    terminal: RtTerminal<CrosstermBackend<std::io::Stdout>>,
}

impl Terminal {
    /// Puts the terminal on the alternate screen in raw mode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotATerminal`] when standard input or output is not a terminal, so a run in
    /// a pipe leaves the screen exactly as it found it, and [`Error::Terminal`] when raw mode or the
    /// alternate screen cannot be entered.
    pub fn open() -> Result<Self, Error> {
        if !both_are_terminals(
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
        ) {
            return Err(Error::NotATerminal);
        }

        enable_raw_mode().map_err(Error::Terminal)?;
        execute!(std::io::stdout(), EnterAlternateScreen).map_err(Error::Terminal)?;
        let terminal =
            RtTerminal::new(CrosstermBackend::new(std::io::stdout())).map_err(Error::Terminal)?;

        Ok(Self { terminal })
    }

    /// Hands the shell back a usable terminal.
    fn leave(&mut self) -> std::io::Result<()> {
        disable_raw_mode()?;
        execute!(std::io::stdout(), LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;

        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // An early return from the driver still hands the shell back a usable terminal.
        let _ = self.leave();
    }
}

impl Screen for Terminal {
    fn poll(&mut self) -> Result<Option<Event>, Error> {
        if !event::poll(Duration::ZERO).map_err(Error::Terminal)? {
            return Ok(None);
        }

        self.event().map(Some)
    }

    fn event(&mut self) -> Result<Event, Error> {
        loop {
            if let Some(event) = classified(&event::read().map_err(Error::Terminal)?) {
                return Ok(event);
            }
        }
    }

    fn draw(
        &mut self,
        view: &View,
    ) -> Result<(), Error> {
        self.terminal
            .draw(|frame| render(frame, view))
            .map_err(Error::Terminal)?;

        Ok(())
    }
}

/// Whether both ends the screen needs are terminals, so a pipe on either end leaves the shell's
/// screen exactly as it was found.
const fn both_are_terminals(
    input: bool,
    output: bool,
) -> bool {
    input && output
}

/// The conversation's event for what the terminal reported, when it acts on it.
///
/// Everything else the terminal can report is dropped here rather than by the driver, which has no
/// answer for an event that moves nothing.
fn classified(event: &CtEvent) -> Option<Event> {
    match event {
        CtEvent::Key(key) => mapped(key).map(Event::Key),
        CtEvent::Resize(..) => Some(Event::Resize),
        _ => None,
    }
}

/// The conversation's key for a reported key event, when it listens for it.
///
/// A control or alt chord that is not one of the mapped ones is dropped rather than typed as its
/// letter, so a human reaching for a key the program does not bind finds no stray character in the
/// line; a shifted character still types itself. A release and a repeat are the same key already
/// reported, so only a press is the stroke.
fn mapped(key: &KeyEvent) -> Option<Key> {
    if key.kind != KeyEventKind::Press {
        return None;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Some(Key::Interrupt),
            KeyCode::Char('n') => Some(Key::New),
            KeyCode::Char('o') => Some(Key::List),
            _ => None,
        };
    }

    if key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }

    match key.code {
        KeyCode::Char(character) => Some(Key::Char(character)),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Tab => Some(Key::Next),
        KeyCode::Esc => Some(Key::Escape),
        _ => None,
    }
}

/// The style a tone reads in.
///
/// The palette is the terminal's own, so a light and a dark theme both stay legible; only the
/// accent and code reach for a named color.
const fn tone_style(tone: Tone) -> Style {
    match tone {
        Tone::Human | Tone::Strong | Tone::Command => Style::new().add_modifier(Modifier::BOLD),
        Tone::Agent => Style::new(),
        Tone::Observation | Tone::Muted => Style::new().add_modifier(Modifier::DIM),
        Tone::Code => Style::new().fg(Color::Cyan),
        Tone::Trouble => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        Tone::Accent => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    }
}

fn text_span(span: Span) -> TextSpan<'static> {
    TextSpan::styled(span.text, tone_style(span.tone))
}

/// Lays out one frame: the header, an optional rule, the body, the composer, and the status row.
///
/// The body is the pane the human opened when he opened one, and the transcript otherwise, so the
/// renderer asks [`View::body`] which rows to draw and decides nothing about them.
fn render(
    frame: &mut Frame<'_>,
    view: &View,
) {
    let area = frame.area();
    let height = area.height;
    let typing = view.typing(usize::from(area.width));
    let composer_height = frame_rows(height, typing.rows.len());

    let mut constraints = vec![Constraint::Length(1)];
    if height >= 10 {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    if height >= 3 {
        constraints.push(Constraint::Length(composer_height));
    }
    if height >= 5 {
        constraints.push(Constraint::Length(1));
    }

    let areas = Layout::vertical(constraints).split(area);
    let mut next = 0;
    let header = areas[next];
    next += 1;
    let rule = (height >= 10).then(|| {
        let rule = areas[next];
        next += 1;

        rule
    });
    let body = areas[next];
    next += 1;
    let composer = (height >= 3).then(|| {
        let composer = areas[next];
        next += 1;

        composer
    });
    let footer = (height >= 5).then(|| areas[next]);

    frame.render_widget(
        Paragraph::new(header_line(view, usize::from(header.width))),
        header,
    );

    if let Some(rule_area) = rule {
        let rule_text = RULE.to_string().repeat(usize::from(rule_area.width));
        frame.render_widget(
            Paragraph::new(TextLine::styled(rule_text, tone_style(Tone::Muted))),
            rule_area,
        );
    }

    let rows: Vec<TextLine<'static>> = view
        .body(usize::from(body.width), usize::from(body.height))
        .into_iter()
        .map(|row| body_line(row, usize::from(body.width)))
        .collect();
    frame.render_widget(Paragraph::new(rows), body);

    let cursor = composer.map_or(Position::new(0, 0), |composer| {
        let first = typing
            .rows
            .len()
            .saturating_sub(usize::from(composer.height));
        let rows: Vec<TextLine<'static>> = typing.rows[first..]
            .iter()
            .cloned()
            .map(|row| body_line(row, usize::from(composer.width)))
            .collect();
        frame.render_widget(Paragraph::new(rows), composer);

        let row = typing.row.saturating_sub(first);
        let column = typing
            .column
            .min(usize::from(composer.width).saturating_sub(1));

        Position::new(
            composer
                .x
                .saturating_add(u16::try_from(column).unwrap_or(u16::MAX)),
            composer
                .y
                .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
        )
    });

    if let Some(footer) = footer {
        frame.render_widget(Paragraph::new(footer_line(view, footer.width)), footer);
    }

    frame.set_cursor_position(cursor);
}

/// The rows the composer takes when the frame is `height` rows tall and the typed line needs
/// `typing` rows. The body keeps at least one row.
fn frame_rows(
    height: u16,
    typing: usize,
) -> u16 {
    let chrome = 1 + usize::from(height >= 10) + usize::from(height >= 5);
    let room = usize::from(height).saturating_sub(chrome + 1).max(1);
    let rows = typing.min(room).max(1);

    u16::try_from(rows).unwrap_or(u16::MAX)
}

fn header_line(
    view: &View,
    width: usize,
) -> TextLine<'static> {
    let mut spans = vec![TextSpan::styled("mokmok", tone_style(Tone::Strong))];
    let mut used = 6;
    push_header_field(
        &mut spans,
        &mut used,
        width,
        &view.short_session(),
        Tone::Muted,
    );

    if let Some(goal) = &view.goal {
        push_header_field(&mut spans, &mut used, width, goal, Tone::Agent);
    }
    if let Some(background) = &view.background {
        push_header_field(
            &mut spans,
            &mut used,
            width,
            &format!("· {background}"),
            Tone::Muted,
        );
    }

    TextLine::from(spans)
}

fn push_header_field(
    spans: &mut Vec<TextSpan<'static>>,
    used: &mut usize,
    width: usize,
    text: &str,
    tone: Tone,
) {
    let room = width.saturating_sub(*used + 2);
    if room == 0 {
        return;
    }

    let text = clipped(text, room);
    if text.is_empty() {
        return;
    }

    spans.push(TextSpan::styled("  ", tone_style(Tone::Muted)));
    *used += 2 + text.width();
    spans.push(TextSpan::styled(text, tone_style(tone)));
}

fn body_line(
    row: Row,
    width: usize,
) -> TextLine<'static> {
    let selected = row.selected;
    let mut spans: Vec<TextSpan<'static>> = row.spans.into_iter().map(text_span).collect();

    if selected {
        let used: usize = spans.iter().map(TextSpan::width).sum();
        if used < width {
            spans.push(TextSpan::styled(
                " ".repeat(width - used),
                Style::new().add_modifier(Modifier::REVERSED),
            ));
        }
    }

    let line = TextLine::from(spans);

    if selected {
        line.style(Style::new().add_modifier(Modifier::REVERSED))
    } else {
        line
    }
}

fn footer_line(
    view: &View,
    width: u16,
) -> TextLine<'static> {
    let width = usize::from(width);
    let mut spans = Vec::new();
    let mut used = 0;

    if let Some(phase) = view.spinner {
        let glyph = SPINNER[usize::from(phase) % SPINNER.len()];
        let spinner = format!("{glyph} ");
        used += spinner.width();
        spans.push(TextSpan::styled(spinner, tone_style(Tone::Accent)));
    }

    let status = clipped(&view.status, width.saturating_sub(used));
    used += status.width();
    spans.push(TextSpan::styled(status, tone_style(Tone::Muted)));

    let hints = view.hints.width();
    let gap = width.saturating_sub(used + hints);

    if !view.hints.is_empty() && gap >= 2 {
        spans.push(TextSpan::raw(" ".repeat(gap)));
        spans.push(TextSpan::styled(
            view.hints.clone(),
            tone_style(Tone::Muted),
        ));
    }

    TextLine::from(spans)
}

/// Clips `text` to `width` columns, ending with an ellipsis when it does not fit.
fn clipped(
    text: &str,
    width: usize,
) -> String {
    crate::port::clipped(text, width)
}
#[cfg(test)]
mod tests {
    use ratatui::Terminal as RtTerminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{
        Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    };
    use ratatui::layout::Position;
    use ratatui::style::Modifier;
    use uuid::Uuid;

    use super::{body_line, both_are_terminals, classified, frame_rows, mapped, render};
    use crate::port::{Entry, Event, Key, Line, Row, Sheet, Span, Tone, View};
    use mokmok_protocol::SessionID;

    #[test]
    fn a_control_key_is_dropped_and_a_shifted_character_types_itself() {
        assert_eq!(
            mapped(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-A must not type an 'a'",
        );
        assert_eq!(
            mapped(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT)),
            None,
            "Alt-A must not type an 'a'",
        );
        assert_eq!(
            mapped(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Key::Interrupt),
            "Ctrl-C is the mapped interrupt",
        );
        assert_eq!(
            mapped(&KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(Key::New),
        );
        assert_eq!(
            mapped(&KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(Key::List),
        );
        assert_eq!(
            mapped(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(Key::Char('a')),
            "an unmodified character still types itself",
        );
        assert_eq!(
            mapped(&KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            Some(Key::Char('A')),
            "a shifted character still types itself",
        );
        assert_eq!(
            mapped(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Key::Enter),
        );
    }

    #[test]
    fn the_pane_keys_and_the_cursor_keys_map_to_themselves() {
        for (code, key) in [(KeyCode::Tab, Key::Next), (KeyCode::Esc, Key::Escape)] {
            assert_eq!(
                mapped(&KeyEvent::new(code, KeyModifiers::NONE)),
                Some(key),
                "{code:?} is a key the conversation acts on",
            );
        }
    }

    #[test]
    fn a_key_that_is_not_pressed_is_not_the_conversation_s_key() {
        assert_eq!(
            mapped(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(Key::Char('a')),
        );
        assert_eq!(
            mapped(&KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )),
            None,
            "a release is the key coming back up, not a second stroke",
        );
    }

    #[test]
    fn the_line_and_scroll_keys_map_to_themselves() {
        for (code, key) in [
            (KeyCode::Backspace, Key::Backspace),
            (KeyCode::Up, Key::Up),
            (KeyCode::Down, Key::Down),
        ] {
            assert_eq!(
                mapped(&KeyEvent::new(code, KeyModifiers::NONE)),
                Some(key),
                "{code:?} is a key the conversation acts on",
            );
        }
    }

    #[test]
    fn a_reported_event_becomes_the_one_the_conversation_acts_on() {
        assert_eq!(
            classified(&CtEvent::Resize(10, 20)),
            Some(Event::Resize),
            "a resize redraws the frame",
        );
        assert_eq!(
            classified(&CtEvent::Key(KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
            ))),
            Some(Event::Key(Key::Char('a'))),
        );
        assert_eq!(
            classified(&CtEvent::Key(KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ))),
            None,
        );
        assert_eq!(
            classified(&CtEvent::FocusGained),
            None,
            "a key press and a resize are the only two the conversation acts on",
        );
    }

    #[test]
    fn a_screen_needs_both_ends_to_be_terminals() {
        assert!(both_are_terminals(true, true));
        assert!(!both_are_terminals(true, false));
        assert!(!both_are_terminals(false, true));
        assert!(!both_are_terminals(false, false));
    }

    fn row(
        buffer: &Buffer,
        y: u16,
        width: u16,
    ) -> String {
        (0..width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn view(
        lines: Vec<Line>,
        scroll: u16,
    ) -> View {
        View {
            session: SessionID::from_uuid(Uuid::from_u128(1)),
            goal: None,
            checkout: None,
            lines,
            input: "hi".to_owned(),
            status: "your turn".to_owned(),
            hints: String::new(),
            placeholder: String::new(),
            scroll,
            background: None,
            sheet: None,
            spinner: None,
        }
    }

    fn draw(
        view: &View,
        width: u16,
        height: u16,
    ) -> RtTerminal<TestBackend> {
        let mut terminal =
            RtTerminal::new(TestBackend::new(width, height)).expect("the screen opens");
        terminal
            .draw(|frame| render(frame, view))
            .expect("the frame draws");

        terminal
    }

    #[test]
    fn the_composer_takes_its_rows_up_to_the_room_the_body_leaves() {
        assert_eq!(frame_rows(10, 1), 1);
        assert_eq!(frame_rows(10, 100), 6);
        assert_eq!(frame_rows(5, 100), 2);
        assert_eq!(frame_rows(4, 100), 2);
        assert_eq!(frame_rows(3, 100), 1);
        assert_eq!(frame_rows(2, 100), 1);
    }

    #[test]
    fn a_selected_row_is_padded_and_reversed() {
        let row = Row {
            spans: vec![Span {
                text: "› entry".to_owned(),
                tone: Tone::Accent,
            }],
            selected: true,
        };
        let line = body_line(row, 10);

        assert_eq!(line.width(), 10);
        let fill = line.spans.last().expect("a fill span");
        assert_eq!(fill.content, "   ");
        assert!(fill.style.add_modifier.contains(Modifier::REVERSED));

        let exact = Row {
            spans: vec![Span {
                text: "0123456789".to_owned(),
                tone: Tone::Agent,
            }],
            selected: true,
        };

        assert_eq!(
            body_line(exact, 10).spans.len(),
            1,
            "a full row needs no empty fill",
        );
    }

    #[test]
    fn the_frame_shows_the_header_the_rule_the_transcript_the_input_and_the_status() {
        let view = view(
            vec![
                Line::Human("what is here".to_owned()),
                Line::Work {
                    command: "ls".to_owned(),
                    observation: "out.txt".to_owned(),
                },
                Line::Agent("an answer".to_owned()),
            ],
            0,
        );
        let terminal = draw(&view, 80, 10);
        let buffer = terminal.backend().buffer();

        assert_eq!(row(buffer, 0, 80), "mokmok  00000000");
        assert_eq!(
            row(buffer, 1, 80),
            "─".repeat(80),
            "the rule separates the heading from the conversation",
        );
        assert_eq!(row(buffer, 2, 80), "› what is here");
        assert_eq!(row(buffer, 3, 80), "  $ ls");
        assert_eq!(row(buffer, 4, 80), "    out.txt");
        assert_eq!(row(buffer, 5, 80), "• an answer");
        assert_eq!(row(buffer, 8, 80), "› hi");
        assert_eq!(row(buffer, 9, 80), "your turn");
        assert_eq!(
            terminal.backend().cursor_position(),
            Position::new(4, 8),
            "the cursor sits one column past the typed line",
        );
    }

    #[test]
    fn the_frame_scrolls_the_transcript_back() {
        let view = view(
            vec![
                Line::Human("oldest".to_owned()),
                Line::Agent("middle".to_owned()),
                Line::Agent("newest".to_owned()),
            ],
            1,
        );
        let terminal = draw(&view, 40, 10);
        let buffer = terminal.backend().buffer();

        assert_eq!(row(buffer, 2, 40), "› oldest");
        assert_eq!(row(buffer, 3, 40), "• middle");
        assert_eq!(row(buffer, 4, 40), "");
        assert_eq!(
            terminal.backend().cursor_position(),
            Position::new(4, 8),
            "the cursor stays on the input row",
        );
    }

    #[test]
    fn a_command_with_a_newline_is_not_misquoted() {
        let view = view(
            vec![Line::Work {
                command: "printf '1\n1\n2\n3\n5\n'".to_owned(),
                observation: String::new(),
            }],
            0,
        );
        let terminal = draw(&view, 80, 10);

        assert_eq!(
            row(terminal.backend().buffer(), 2, 80),
            r"  $ printf '1\n1\n2\n3\n5\n'",
            "the drawn row escapes each newline instead of collapsing the command",
        );
    }

    #[test]
    fn a_work_row_shows_the_command_and_its_outcome() {
        let reported = view(
            vec![
                Line::Work {
                    command: "ls".to_owned(),
                    observation: "exit code 0\nstdout: ok\n".to_owned(),
                },
                Line::Agent("next".to_owned()),
            ],
            0,
        );
        let terminal = draw(&reported, 80, 10);
        {
            let buffer = terminal.backend().buffer();
            assert_eq!(row(buffer, 2, 80), "  $ ls");
            assert_eq!(row(buffer, 3, 80), "    exit code 0");
            assert_eq!(row(buffer, 4, 80), "• next");
        }

        let silent = view(
            vec![
                Line::Work {
                    command: "ls".to_owned(),
                    observation: String::new(),
                },
                Line::Agent("next".to_owned()),
            ],
            0,
        );
        let terminal = draw(&silent, 80, 10);
        let buffer = terminal.backend().buffer();

        assert_eq!(row(buffer, 2, 80), "  $ ls");
        assert_eq!(row(buffer, 3, 80), "• next");
    }

    #[test]
    fn a_find_row_shows_the_query_and_its_answer() {
        let view = view(
            vec![
                Line::Find {
                    query: "quokka".to_owned(),
                    observation: "indexed=2 hits=1\n1.500  notes.txt\n".to_owned(),
                },
                Line::Agent("next".to_owned()),
            ],
            0,
        );
        let terminal = draw(&view, 80, 10);
        let buffer = terminal.backend().buffer();

        assert_eq!(row(buffer, 2, 80), "  ⌕ quokka");
        assert_eq!(row(buffer, 3, 80), "    indexed=2 hits=1");
        assert_eq!(row(buffer, 4, 80), "• next");
    }

    #[test]
    fn the_newest_rows_survive_a_transcript_taller_than_the_pane() {
        let view = view(
            vec![
                Line::Human("first".to_owned()),
                Line::Agent("second".to_owned()),
                Line::Human("third".to_owned()),
                Line::Agent("fourth".to_owned()),
                Line::Human("fifth".to_owned()),
                Line::Agent("sixth".to_owned()),
                Line::Human("seventh".to_owned()),
            ],
            0,
        );
        let terminal = draw(&view, 40, 10);
        let buffer = terminal.backend().buffer();

        assert_eq!(row(buffer, 2, 40), "• second");
        assert_eq!(row(buffer, 3, 40), "› third");
        assert_eq!(row(buffer, 4, 40), "• fourth");
        assert_eq!(row(buffer, 5, 40), "› fifth");
        assert_eq!(row(buffer, 6, 40), "• sixth");
        assert_eq!(row(buffer, 7, 40), "› seventh");
        assert_eq!(row(buffer, 8, 40), "› hi");
    }

    fn entry(
        seed: u128,
        goal: Option<&str>,
        held: bool,
    ) -> Entry {
        Entry {
            session: SessionID::from_uuid(Uuid::from_u128(seed << 120)),
            goal: goal.map(str::to_owned),
            status: "your turn".to_owned(),
            held,
            focused: false,
        }
    }

    #[test]
    fn the_listing_draws_its_entries_and_takes_the_composer_row() {
        let view = View {
            sheet: Some(Sheet::List {
                entries: vec![
                    entry(1, Some("hold a conversation"), true),
                    entry(2, None, false),
                ],
                cursor: 1,
            }),
            ..view(Vec::new(), 0)
        };
        let terminal = draw(&view, 80, 10);
        let buffer = terminal.backend().buffer();

        assert_eq!(row(buffer, 1, 80), "─".repeat(80));
        assert_eq!(row(buffer, 2, 80), "sessions");
        assert_eq!(
            row(buffer, 3, 80),
            "  * 01000000  your turn  hold a conversation",
        );
        assert_eq!(
            row(buffer, 4, 80),
            "›   02000000  your turn  (no goal recorded)",
        );
        assert_eq!(row(buffer, 8, 80), "");
        assert_eq!(
            terminal.backend().cursor_position(),
            Position::new(0, 8),
            "the cursor sits at the start of the blank composer row",
        );
    }

    #[test]
    fn naming_a_goal_draws_the_prompt_and_the_cursor_after_the_goal() {
        let view = View {
            sheet: Some(Sheet::New {
                goal: "count".to_owned(),
            }),
            ..view(vec![Line::Human("hello".to_owned())], 0)
        };
        let terminal = draw(&view, 80, 10);

        assert_eq!(row(terminal.backend().buffer(), 2, 80), "› hello");
        assert_eq!(row(terminal.backend().buffer(), 8, 80), "goal› count");
        assert_eq!(
            terminal.backend().cursor_position(),
            Position::new(11, 8),
            "the cursor sits one column past the goal",
        );
    }

    #[test]
    fn an_empty_input_shows_the_placeholder_and_parks_the_cursor_at_the_prompt() {
        let view = View {
            input: String::new(),
            placeholder: "Send a line to the agent…".to_owned(),
            ..view(Vec::new(), 0)
        };
        let terminal = draw(&view, 80, 10);

        assert_eq!(
            row(terminal.backend().buffer(), 8, 80),
            "› Send a line to the agent…",
        );
        assert_eq!(
            terminal.backend().cursor_position(),
            Position::new(2, 8),
            "the cursor sits after the prompt, before the placeholder",
        );
    }

    #[test]
    fn the_cursor_counts_a_wide_character_as_two_columns() {
        let two = View {
            input: "日本".to_owned(),
            ..view(Vec::new(), 0)
        };
        let terminal = draw(&two, 80, 10);

        assert_eq!(terminal.backend().cursor_position(), Position::new(6, 8));

        let three = View {
            input: "日本語".to_owned(),
            ..view(Vec::new(), 0)
        };
        let terminal = draw(&three, 80, 10);

        assert_eq!(
            terminal.backend().cursor_position(),
            Position::new(8, 8),
            "three characters of two columns each sit beside the two-column prompt",
        );
    }

    #[test]
    fn a_line_wider_than_the_pane_wraps_the_input_and_the_cursor_follows() {
        let view = View {
            input: "alpha bravo charlie".to_owned(),
            ..view(vec![Line::Human("hello".to_owned())], 0)
        };
        let terminal = draw(&view, 20, 10);
        let buffer = terminal.backend().buffer();

        assert_eq!(row(buffer, 7, 20), "› alpha bravo");
        assert_eq!(row(buffer, 8, 20), "  charlie");
        assert_eq!(row(buffer, 9, 20), "your turn");
        assert_eq!(
            terminal.backend().cursor_position(),
            Position::new(9, 8),
            "the cursor follows the wrapped line onto its last row",
        );
    }

    #[test]
    fn the_heading_reports_the_other_sessions_that_are_working() {
        let quiet = view(vec![Line::Human("hello".to_owned())], 0);
        let busy = View {
            background: Some("1 other session working".to_owned()),
            ..quiet.clone()
        };

        let terminal = draw(&quiet, 120, 10);
        assert_eq!(
            row(terminal.backend().buffer(), 0, 120),
            "mokmok  00000000",
            "a frame with nothing moving elsewhere is the frame it was",
        );

        let terminal = draw(&busy, 120, 10);
        assert_eq!(
            row(terminal.backend().buffer(), 0, 120),
            "mokmok  00000000  · 1 other session working",
        );
    }

    #[test]
    fn the_heading_keeps_the_goal_and_truncates_it_to_the_width() {
        let view = View {
            goal: Some("hold a conversation with the constrained agent".to_owned()),
            ..view(vec![Line::Human("hello".to_owned())], 0)
        };
        let terminal = draw(&view, 30, 10);
        let heading = row(terminal.backend().buffer(), 0, 30);

        assert_eq!(heading, "mokmok  00000000  hold a conv…");
    }

    #[test]
    fn the_footer_draws_the_spinner_while_the_session_is_working() {
        let view = View {
            status: "the agent is working".to_owned(),
            hints: "enter send".to_owned(),
            spinner: Some(3),
            ..view(Vec::new(), 0)
        };
        let terminal = draw(&view, 60, 10);
        let footer = row(terminal.backend().buffer(), 9, 60);

        assert_eq!(
            footer,
            format!("⠸ the agent is working{}enter send", " ".repeat(28))
        );
    }

    #[test]
    fn the_footer_drops_the_hints_when_they_do_not_fit() {
        let view = View {
            status: "the agent is working".to_owned(),
            hints: "enter send  tab next  ^O sessions  ^N new  ^C quit".to_owned(),
            spinner: Some(0),
            ..view(Vec::new(), 0)
        };
        let terminal = draw(&view, 20, 10);
        let footer = row(terminal.backend().buffer(), 9, 20);

        assert_eq!(footer, "⠋ the agent is work…");
        assert!(!footer.contains("enter"), "a clipped hint is dropped whole");
    }

    #[test]
    fn the_footer_keeps_hints_at_two_columns_and_drops_them_below() {
        let view = View {
            status: "the agent is working".to_owned(),
            hints: "enter send".to_owned(),
            spinner: Some(0),
            ..view(Vec::new(), 0)
        };
        let terminal = draw(&view, 34, 10);

        assert_eq!(
            row(terminal.backend().buffer(), 9, 34),
            "⠋ the agent is working  enter send",
        );

        let terminal = draw(&view, 33, 10);

        assert_eq!(
            row(terminal.backend().buffer(), 9, 33),
            "⠋ the agent is working",
            "one column of gap is not enough to separate the hints",
        );
    }

    #[test]
    fn a_long_reply_wraps_inside_the_body() {
        let view = view(
            vec![Line::Agent(
                "one two three four five six seven eight".to_owned(),
            )],
            0,
        );
        let terminal = draw(&view, 20, 10);
        let buffer = terminal.backend().buffer();

        for y in 2..8 {
            let line = row(buffer, y, 20);
            assert!(line.chars().count() <= 20, "row {y} fits: {line:?}");
        }
        assert!(row(buffer, 2, 20).starts_with('•'));
    }

    #[test]
    fn a_short_screen_keeps_the_header_the_body_and_the_composer() {
        let view = view(vec![Line::Human("hello".to_owned())], 0);
        let terminal = draw(&view, 40, 4);
        let buffer = terminal.backend().buffer();

        assert_eq!(row(buffer, 0, 40), "mokmok  00000000");
        assert_eq!(row(buffer, 1, 40), "› hello");
        assert_eq!(row(buffer, 3, 40), "› hi");
    }
}
