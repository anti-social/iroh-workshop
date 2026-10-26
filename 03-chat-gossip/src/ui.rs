use iroh::EndpointId;
use iroh_gossip::proto::TopicId;
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Paragraph, Wrap},
};
use tui_textarea::TextArea;

/// Colors used for other people's names, picked by their endpoint id.
const NAME_COLORS: [Color; 6] = [
    Color::Cyan,
    Color::Magenta,
    Color::Yellow,
    Color::Blue,
    Color::LightRed,
    Color::LightGreen,
];

/// Maximum height of the input box, including its borders.
const MAX_INPUT_HEIGHT: u16 = 10;

pub enum Entry {
    Peer {
        id: EndpointId,
        name: String,
        text: String,
    },
    Me(String),
    Info(String),
}

/// What the event loop should do after a key press.
pub enum Action {
    None,
    Send(String),
    Quit,
}

pub struct App {
    topic: TopicId,
    entries: Vec<Entry>,
    input: TextArea<'static>,
    pub neighbors: usize,
    /// First visible history line, or `None` to follow the newest messages.
    scroll: Option<usize>,
    /// Entries added while scrolled up.
    unseen: usize,
    /// History height and maximum scroll offset, as of the last draw.
    page: usize,
    max_scroll: usize,
}

impl App {
    pub fn new(topic: TopicId) -> Self {
        Self {
            topic,
            entries: Vec::new(),
            input: new_input(),
            neighbors: 0,
            scroll: None,
            unseen: 0,
            page: 0,
            max_scroll: 0,
        }
    }

    pub fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
        if self.scroll.is_some() {
            self.unseen += 1;
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push(Entry::Info(text.into()));
    }

    pub fn clear_input(&mut self) {
        self.input = new_input();
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Action::Quit,
            KeyCode::Char('c') if ctrl => return Action::Quit,
            // Terminals without keyboard enhancement send Ctrl+J for Ctrl+Enter.
            KeyCode::Char('j') if ctrl => {
                self.input.insert_newline();
            }
            KeyCode::Enter if key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
                self.input.insert_newline();
            }
            KeyCode::Enter => {
                // A trailing backslash continues the message on a new line.
                let (row, col) = self.input.cursor();
                if col > 0 && self.input.lines()[row][..col].ends_with('\\') {
                    self.input.delete_char();
                    self.input.insert_newline();
                } else {
                    let text = self.input.lines().join("\n");
                    if !text.trim().is_empty() {
                        return Action::Send(text);
                    }
                }
            }
            KeyCode::PageUp => self.scroll_by(-(self.page.max(1) as isize)),
            KeyCode::PageDown => self.scroll_by(self.page.max(1) as isize),
            KeyCode::Home if ctrl => self.scroll = Some(0),
            KeyCode::End if ctrl => self.scroll_to_bottom(),
            _ => {
                self.input.input(key);
            }
        }
        Action::None
    }

    fn scroll_by(&mut self, delta: isize) {
        let top = self.scroll.unwrap_or(self.max_scroll);
        let top = top.saturating_add_signed(delta);
        if top >= self.max_scroll {
            self.scroll_to_bottom();
        } else {
            self.scroll = Some(top);
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll = None;
        self.unseen = 0;
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let input_height = (self.input.lines().len() as u16 + 2).min(MAX_INPUT_HEIGHT);
        let [history_area, input_area, footer_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let history = Paragraph::new(self.history_text()).wrap(Wrap { trim: false });
        self.page = history_area.height as usize;
        self.max_scroll = history
            .line_count(history_area.width)
            .saturating_sub(self.page);
        let top = match self.scroll {
            Some(top) if top < self.max_scroll => top,
            _ => {
                self.scroll_to_bottom();
                self.max_scroll
            }
        };
        frame.render_widget(history.scroll((top as u16, 0)), history_area);

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::DarkGray));
        let inner = block.inner(input_area);
        frame.render_widget(block, input_area);
        let [prompt_area, text_area] =
            Layout::horizontal([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        frame.render_widget(Paragraph::new(">").bold(), prompt_area);
        frame.render_widget(&self.input, text_area);

        let status = self.status_line();
        let [hints_area, status_area] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(status.width() as u16),
        ])
        .areas(footer_area);
        let hints = Line::from(
            " enter send · shift+enter newline · pgup/pgdn scroll · esc quit".dark_gray(),
        );
        frame.render_widget(hints, hints_area);
        frame.render_widget(status, status_area);
    }

    fn history_text(&self) -> Text<'static> {
        let mut text = Text::default();
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 {
                text.push_line(Line::default());
            }
            match entry {
                Entry::Peer { id, name, text: body } => {
                    let color = NAME_COLORS[id.as_bytes()[0] as usize % NAME_COLORS.len()];
                    text.push_line(Line::from(vec![
                        Span::styled("● ", Style::new().fg(color)),
                        Span::styled(name.clone(), Style::new().fg(color).bold()),
                    ]));
                    for line in body.lines() {
                        text.push_line(Line::from(format!("  {line}")));
                    }
                }
                Entry::Me(body) => {
                    let style = Style::new().fg(Color::Gray).add_modifier(Modifier::DIM);
                    for (i, line) in body.lines().enumerate() {
                        let head = if i == 0 { "> " } else { "  " };
                        text.push_line(Line::styled(format!("{head}{line}"), style));
                    }
                }
                Entry::Info(body) => {
                    for (i, line) in body.lines().enumerate() {
                        let head = if i == 0 { "  ⎿ " } else { "    " };
                        text.push_line(Line::from(format!("{head}{line}").dark_gray()));
                    }
                }
            }
        }
        text
    }

    fn status_line(&self) -> Line<'static> {
        let mut spans = Vec::new();
        if self.unseen > 0 {
            spans.push(format!("↓ {} new", self.unseen).black().on_yellow());
            spans.push(" · ".dark_gray());
        }
        spans.push(format!("room {}", self.topic.fmt_short()).dark_gray());
        spans.push(" · ".dark_gray());
        spans.push(match self.neighbors {
            0 => "waiting for peers".yellow(),
            1 => "1 neighbor".green(),
            n => format!("{n} neighbors").green(),
        });
        spans.push(Span::raw(" "));
        Line::from(spans)
    }
}

fn new_input() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_cursor_line_style(Style::default());
    input.set_placeholder_text("Type a message");
    input
}
