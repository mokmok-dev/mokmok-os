//! The ratatui adapter: crossterm events in, one frame out.

use std::io::IsTerminal;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Position};
use ratatui::text::Line as TextLine;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal as RtTerminal};

use crate::Error;
use crate::port::{Event, Key, Screen, View};

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
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
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
    fn event(&mut self) -> Result<Event, Error> {
        loop {
            match event::read().map_err(Error::Terminal)? {
                CtEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(key) = mapped(&key) {
                        return Ok(Event::Key(key));
                    }
                },
                CtEvent::Resize(..) => return Ok(Event::Resize),
                _ => {},
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

/// The conversation's key for a reported key event, when it listens for it.
///
/// A control or alt chord that is not the mapped `Ctrl-C` is dropped rather than typed as its
/// letter, so a human reaching for a key the program does not bind finds no stray character in the
/// line; a shifted character still types itself.
fn mapped(key: &KeyEvent) -> Option<Key> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Key::Interrupt);
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }

    match key.code {
        KeyCode::Char(character) => Some(Key::Char(character)),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        _ => None,
    }
}

/// Lays out one frame: the header, the transcript, the line being typed, and the status line.
fn render(
    frame: &mut Frame<'_>,
    view: &View,
) {
    let [header, body, input, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let heading = format!("mokmok-tui  {}  {}", view.session.as_uuid(), view.status);
    frame.render_widget(Paragraph::new(TextLine::raw(heading)), header);

    let lines: Vec<TextLine<'static>> = view
        .visible(usize::from(body.height))
        .into_iter()
        .map(TextLine::raw)
        .collect();
    frame.render_widget(Paragraph::new(lines), body);

    let typed = format!("> {}", view.input);
    frame.render_widget(Paragraph::new(TextLine::raw(typed.as_str())), input);
    frame.render_widget(Paragraph::new(TextLine::raw(view.status.as_str())), status);

    // `Screen::draw` hands over a `View` and nothing else, so the adapter cannot tell a waiting fold
    // from a working one; parking the cursor on the input row is the one cursor the frame can place.
    let column = u16::try_from(typed.chars().count()).unwrap_or(u16::MAX);
    frame.set_cursor_position(Position::new(input.x.saturating_add(column), input.y));
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal as RtTerminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Position;
    use uuid::Uuid;

    use super::{mapped, render};
    use crate::port::{Key, Line, View};
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
            "Ctrl-C is the one mapped control key",
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
            lines,
            input: "hi".to_owned(),
            status: "your turn".to_owned(),
            scroll,
        }
    }

    #[test]
    fn the_frame_shows_the_header_the_transcript_the_input_and_the_status() {
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
        let mut terminal = RtTerminal::new(TestBackend::new(80, 10)).expect("the screen opens");
        terminal
            .draw(|frame| render(frame, &view))
            .expect("the frame draws");
        let buffer = terminal.backend().buffer();

        assert_eq!(
            row(buffer, 0, 80),
            format!("mokmok-tui  {}  your turn", view.session.as_uuid()),
        );
        assert_eq!(row(buffer, 1, 80), "you    what is here");
        assert_eq!(row(buffer, 2, 80), "  $ ls");
        assert_eq!(row(buffer, 3, 80), "    out.txt");
        assert_eq!(row(buffer, 4, 80), "agent  an answer");
        assert_eq!(row(buffer, 8, 80), "> hi");
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
        let mut terminal = RtTerminal::new(TestBackend::new(40, 10)).expect("the screen opens");
        terminal
            .draw(|frame| render(frame, &view))
            .expect("the frame draws");
        let buffer = terminal.backend().buffer();

        assert_eq!(row(buffer, 1, 40), "you    oldest");
        assert_eq!(row(buffer, 2, 40), "agent  middle");
        assert_eq!(row(buffer, 3, 40), "");
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
        let mut terminal = RtTerminal::new(TestBackend::new(80, 10)).expect("the screen opens");
        terminal
            .draw(|frame| render(frame, &view))
            .expect("the frame draws");
        let buffer = terminal.backend().buffer();

        assert_eq!(
            row(buffer, 1, 80),
            r"  $ printf '1\n1\n2\n3\n5\n'",
            "the drawn row escapes each newline instead of collapsing the command",
        );
    }

    #[test]
    fn a_work_row_shows_the_command_and_its_outcome() {
        let mut terminal = RtTerminal::new(TestBackend::new(80, 10)).expect("the screen opens");

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
        terminal
            .draw(|frame| render(frame, &reported))
            .expect("the frame draws");
        {
            let buffer = terminal.backend().buffer();
            assert_eq!(row(buffer, 1, 80), "  $ ls");
            assert_eq!(row(buffer, 2, 80), "    exit code 0");
            assert_eq!(row(buffer, 3, 80), "agent  next");
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
        terminal
            .draw(|frame| render(frame, &silent))
            .expect("the frame draws");
        {
            let buffer = terminal.backend().buffer();
            assert_eq!(row(buffer, 1, 80), "  $ ls");
            assert_eq!(row(buffer, 2, 80), "agent  next");
        }
    }

    #[test]
    fn the_newest_rows_survive_a_transcript_taller_than_the_pane() {
        let view = view(
            vec![
                Line::Human("first".to_owned()),
                Line::Work {
                    command: "ls".to_owned(),
                    observation: "out.txt".to_owned(),
                },
                Line::Agent("second".to_owned()),
                Line::Human("third".to_owned()),
                Line::Agent("fourth".to_owned()),
                Line::Human("fifth".to_owned()),
                Line::Agent("sixth".to_owned()),
                Line::Human("seventh".to_owned()),
            ],
            0,
        );
        let mut terminal = RtTerminal::new(TestBackend::new(40, 10)).expect("the screen opens");
        terminal
            .draw(|frame| render(frame, &view))
            .expect("the frame draws");
        let buffer = terminal.backend().buffer();

        assert_eq!(row(buffer, 1, 40), "    out.txt");
        assert_eq!(row(buffer, 2, 40), "agent  second");
        assert_eq!(row(buffer, 3, 40), "you    third");
        assert_eq!(row(buffer, 4, 40), "agent  fourth");
        assert_eq!(row(buffer, 5, 40), "you    fifth");
        assert_eq!(row(buffer, 6, 40), "agent  sixth");
        assert_eq!(row(buffer, 7, 40), "you    seventh");
        assert_eq!(row(buffer, 8, 40), "> hi");
    }

    #[test]
    fn a_row_count_that_counts_rows_and_not_lines() {
        let lines = || {
            vec![
                Line::Human("first".to_owned()),
                Line::Work {
                    command: "ls".to_owned(),
                    observation: "out.txt".to_owned(),
                },
                Line::Agent("last".to_owned()),
            ]
        };
        let mut terminal = RtTerminal::new(TestBackend::new(40, 5)).expect("the screen opens");

        let newest = view(lines(), 0);
        terminal
            .draw(|frame| render(frame, &newest))
            .expect("the frame draws");
        {
            let buffer = terminal.backend().buffer();
            assert_eq!(row(buffer, 1, 40), "    out.txt");
            assert_eq!(row(buffer, 2, 40), "agent  last");
        }

        let one_back = view(lines(), 1);
        terminal
            .draw(|frame| render(frame, &one_back))
            .expect("the frame draws");
        {
            let buffer = terminal.backend().buffer();
            assert_eq!(row(buffer, 1, 40), "  $ ls");
            assert_eq!(row(buffer, 2, 40), "    out.txt");
        }
    }
}
