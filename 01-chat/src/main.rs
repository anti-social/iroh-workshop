use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use iroh::{Endpoint, EndpointId, SecretKey, endpoint::Connection, endpoint::presets};
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::{
        event::{
            self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
            KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
            PushKeyboardEnhancementFlags,
        },
        execute,
        terminal::supports_keyboard_enhancement,
    },
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
};
use tokio::sync::mpsc;
use tui_textarea::TextArea;

const ALPN: &[u8] = b"iroh-workshop/chat/0";
/// Maximum size of a single chat message in bytes.
const MAX_MESSAGE_SIZE: usize = 64 * 1024;

#[derive(Parser)]
#[command(about = "One-to-one chat over iroh")]
struct Cli {
    /// Path to the secret key file.
    #[arg(long, global = true)]
    key: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate and store a new endpoint key, then print its public part.
    Keygen {
        /// Overwrite an existing key.
        #[arg(long)]
        force: bool,
    },
    /// Wait for a peer to connect.
    Listen,
    /// Connect to a peer by its endpoint id.
    Connect { peer: EndpointId },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let key_path = match cli.key {
        Some(path) => path,
        None => default_key_path()?,
    };

    match cli.command {
        Command::Keygen { force } => keygen(&key_path, force),
        Command::Listen => {
            let endpoint = bind(&key_path).await?;
            println!("Listening. Your endpoint id:\n{}", endpoint.id());
            println!("Waiting for a peer to connect...");
            let conn = loop {
                let incoming = endpoint.accept().await.context("endpoint closed")?;
                match incoming.await {
                    Ok(conn) => break conn,
                    Err(err) => eprintln!("incoming connection failed: {err}"),
                }
            };
            chat(&endpoint, conn).await
        }
        Command::Connect { peer } => {
            let endpoint = bind(&key_path).await?;
            println!("Connecting to {peer}...");
            let conn = endpoint.connect(peer, ALPN).await?;
            chat(&endpoint, conn).await
        }
    }
}

fn default_key_path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("cannot determine config directory")?;
    Ok(dir.join("iroh-chat").join("secret.key"))
}

fn keygen(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!("{} already exists, use --force to overwrite", path.display());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let key = SecretKey::generate();
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .and_then(|mut file| file.write_all(&key.to_bytes()))
        .with_context(|| format!("failed to write {}", path.display()))?;
    println!("Key saved to {}", path.display());
    println!("{}", key.public());
    Ok(())
}

fn load_key(path: &Path) -> Result<SecretKey> {
    let bytes = fs::read(path).with_context(|| {
        format!(
            "failed to read key from {}, run `keygen` first",
            path.display()
        )
    })?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| anyhow::anyhow!("invalid key file"))?;
    Ok(SecretKey::from_bytes(&bytes))
}

async fn bind(key_path: &Path) -> Result<Endpoint> {
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(load_key(key_path)?)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;
    Ok(endpoint)
}

enum Message {
    Peer(String),
    Me(String),
    Info(String),
}

struct App {
    peer: EndpointId,
    messages: Vec<Message>,
    input: TextArea<'static>,
    status: Line<'static>,
}

impl App {
    fn new(peer: EndpointId, enhanced_keys: bool) -> Self {
        let send_hint = if enhanced_keys {
            "Ctrl+Enter"
        } else {
            "Alt+Enter / Ctrl+J"
        };
        Self {
            peer,
            messages: vec![Message::Info(format!(
                "Connected. Enter: new line, {send_hint}: send, Esc: quit"
            ))],
            input: new_input(),
            status: Line::from("connecting..."),
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let input_height = (self.input.lines().len() as u16 + 2).clamp(3, 10);
        let [history_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let mut text = Text::default();
        for message in &self.messages {
            let (label, style, body) = match message {
                Message::Peer(body) => ("peer", Style::new().fg(Color::Cyan), body),
                Message::Me(body) => ("me", Style::new().fg(Color::Green), body),
                Message::Info(body) => ("*", Style::new().fg(Color::DarkGray), body),
            };
            let prefix = format!("{label}: ");
            let indent = " ".repeat(prefix.len());
            for (i, line) in body.lines().enumerate() {
                let head = if i == 0 { &prefix } else { &indent };
                text.push_line(Line::from(vec![
                    Span::styled(head.clone(), style.add_modifier(Modifier::BOLD)),
                    Span::raw(line.to_string()),
                ]));
            }
        }

        let block = Block::bordered().title(format!(" chat with {} ", self.peer.fmt_short()));
        let inner_width = history_area.width.saturating_sub(2);
        let inner_height = history_area.height.saturating_sub(2) as usize;
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        let scroll = paragraph.line_count(inner_width).saturating_sub(inner_height);
        frame.render_widget(
            paragraph.block(block).scroll((scroll as u16, 0)),
            history_area,
        );
        frame.render_widget(&self.input, input_area);
        frame.render_widget(&self.status, status_area);
    }
}

/// Describes the path currently used to send data to the peer.
fn connection_status(conn: &Connection) -> Line<'static> {
    let paths = conn.paths();
    let Some(path) = paths.iter().find(|path| path.is_selected()) else {
        return Line::from(" no path".dark_gray());
    };
    let rtt = path.rtt().as_millis();
    let (kind, color) = if path.is_ip() {
        ("DIRECT", Color::Green)
    } else if path.is_relay() {
        ("RELAYED", Color::Yellow)
    } else {
        ("CUSTOM", Color::Blue)
    };
    let other_paths = paths.len() - 1;
    Line::from(vec![
        Span::styled(format!(" {kind} "), Style::new().fg(Color::Black).bg(color).bold()),
        Span::raw(format!(
            " via {} · rtt {rtt} ms · {other_paths} other path(s)",
            path.remote_addr()
        )),
    ])
}

fn new_input() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_block(Block::bordered().title(" message "));
    input.set_cursor_line_style(Style::default());
    input
}

fn is_send_key(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Enter)
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        // Terminals without keyboard enhancement send Ctrl+J for Ctrl+Enter.
        || key.code == KeyCode::Char('j') && key.modifiers == KeyModifiers::CONTROL
}

async fn chat(endpoint: &Endpoint, conn: Connection) -> Result<()> {
    let peer = conn.remote_id();

    // Receive each message on its own uni stream.
    let (incoming_tx, mut incoming_rx) = mpsc::channel::<String>(16);
    let recv_conn = conn.clone();
    tokio::spawn(async move {
        while let Ok(mut stream) = recv_conn.accept_uni().await {
            let Ok(bytes) = stream.read_to_end(MAX_MESSAGE_SIZE).await else {
                break;
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if incoming_tx.send(text).await.is_err() {
                break;
            }
        }
    });

    // Terminal events are read on a blocking thread.
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(16);
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            if event_tx.blocking_send(event).is_err() {
                break;
            }
        }
    });

    let mut terminal = ratatui::init();
    let enhanced_keys = supports_keyboard_enhancement().unwrap_or(false);
    if enhanced_keys {
        execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    let mut app = App::new(peer, enhanced_keys);
    let result = run(&mut terminal, &mut app, &conn, &mut incoming_rx, &mut event_rx).await;

    if enhanced_keys {
        execute!(std::io::stdout(), PopKeyboardEnhancementFlags)?;
    }
    ratatui::restore();

    conn.close(0u32.into(), b"bye");
    endpoint.close().await;
    result
}

async fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    conn: &Connection,
    incoming_rx: &mut mpsc::Receiver<String>,
    event_rx: &mut mpsc::Receiver<Event>,
) -> Result<()> {
    let mut connected = true;
    let mut status_tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        tokio::select! {
            _ = status_tick.tick(), if connected => app.status = connection_status(conn),
            Some(text) = incoming_rx.recv() => app.messages.push(Message::Peer(text)),
            reason = conn.closed(), if connected => {
                connected = false;
                app.status = Line::from(" DISCONNECTED ".black().on_red().bold());
                app.messages.push(Message::Info(format!(
                    "Connection closed: {reason}. Press Esc to quit."
                )));
            }
            event = event_rx.recv() => {
                let Some(Event::Key(key)) = event else { continue };
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.code == KeyCode::Esc
                    || key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL
                {
                    return Ok(());
                }
                if is_send_key(&key) {
                    let text = app.input.lines().join("\n");
                    if text.trim().is_empty() {
                        continue;
                    }
                    if !connected {
                        app.messages.push(Message::Info("Not connected.".into()));
                        continue;
                    }
                    match send(conn, &text).await {
                        Ok(()) => {
                            app.messages.push(Message::Me(text));
                            app.input = new_input();
                        }
                        Err(err) => app.messages.push(Message::Info(format!("Send failed: {err}"))),
                    }
                } else {
                    app.input.input(key);
                }
            }
        }
    }
}

async fn send(conn: &Connection, text: &str) -> Result<()> {
    if text.len() > MAX_MESSAGE_SIZE {
        bail!("message is too long");
    }
    let mut stream = conn.open_uni().await?;
    stream.write_all(text.as_bytes()).await?;
    stream.finish()?;
    Ok(())
}
