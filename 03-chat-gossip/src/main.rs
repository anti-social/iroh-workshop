mod proto;
mod ui;

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use iroh::{
    Endpoint, EndpointId, SecretKey, address_lookup::memory::MemoryLookup, endpoint::presets,
    protocol::Router,
};
use iroh_gossip::{
    api::{Event as GossipEvent, GossipReceiver, GossipSender},
    net::{GOSSIP_ALPN, Gossip},
    proto::TopicId,
};
use n0_future::StreamExt;
use ratatui::{
    DefaultTerminal,
    crossterm::{
        event::{
            self, Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
            PushKeyboardEnhancementFlags,
        },
        execute,
        terminal::supports_keyboard_enhancement,
    },
};
use tokio::sync::mpsc;

use crate::{
    proto::{Message, Ticket},
    ui::{Action, App, Entry},
};

/// Maximum size of a single encoded gossip message in bytes.
const MAX_MESSAGE_SIZE: usize = 64 * 1024;

#[derive(Parser)]
#[command(about = "Group chat over iroh-gossip")]
struct Cli {
    /// Path to the secret key file.
    #[arg(long, global = true)]
    key: Option<PathBuf>,
    /// Nickname shown to others. Defaults to $USER.
    #[arg(long, global = true)]
    name: Option<String>,
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
    /// Open a new chat room and print a ticket for others to join.
    Open,
    /// Join a chat room using a ticket.
    Join { ticket: Ticket },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let key_path = match cli.key {
        Some(path) => path,
        None => default_key_path()?,
    };

    let (topic, peers) = match cli.command {
        Command::Keygen { force } => return keygen(&key_path, force),
        Command::Open => (TopicId::from_bytes(rand::random()), Vec::new()),
        Command::Join { ticket } => (ticket.topic, ticket.peers),
    };

    let secret_key = load_key(&key_path)?;
    let name = cli
        .name
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| secret_key.public().fmt_short().to_string());

    // Addresses from the ticket are added here, so peers can be dialed directly.
    let memory_lookup = MemoryLookup::new();
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .address_lookup(memory_lookup.clone())
        .bind()
        .await?;
    let gossip = Gossip::builder()
        .max_message_size(MAX_MESSAGE_SIZE)
        .spawn(endpoint.clone());
    let router = Router::builder(endpoint.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .spawn();

    println!("Connecting to the relay...");
    endpoint.online().await;
    let peer_ids: Vec<EndpointId> = peers.iter().map(|peer| peer.id).collect();
    let ticket = Ticket {
        topic,
        peers: peers
            .iter()
            .cloned()
            .chain([endpoint.addr()])
            .collect(),
    };
    for peer in peers {
        memory_lookup.add_endpoint_info(peer);
    }
    println!("Ticket to join this room:\n{ticket}");

    let (sender, receiver) = gossip.subscribe(topic, peer_ids).await?.split();
    let result = chat(&endpoint, topic, &ticket, &name, sender, receiver).await;

    router.shutdown().await?;
    println!("Ticket to join this room:\n{ticket}");
    result
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

/// Gossip events after decoding and signature checks.
enum Incoming {
    Message(EndpointId, Message),
    NeighborUp(EndpointId),
    NeighborDown(EndpointId),
    Lagged,
    Invalid(anyhow::Error),
    Closed(String),
}

async fn receive(mut receiver: GossipReceiver, tx: mpsc::Sender<Incoming>) {
    loop {
        let incoming = match receiver.try_next().await {
            Ok(Some(GossipEvent::Received(msg))) => match proto::verify_and_decode(&msg.content) {
                Ok((from, message)) => Incoming::Message(from, message),
                Err(err) => Incoming::Invalid(err),
            },
            Ok(Some(GossipEvent::NeighborUp(id))) => Incoming::NeighborUp(id),
            Ok(Some(GossipEvent::NeighborDown(id))) => Incoming::NeighborDown(id),
            Ok(Some(GossipEvent::Lagged)) => Incoming::Lagged,
            Ok(None) => Incoming::Closed("subscription ended".into()),
            Err(err) => Incoming::Closed(err.to_string()),
        };
        let closed = matches!(incoming, Incoming::Closed(_));
        if tx.send(incoming).await.is_err() || closed {
            break;
        }
    }
}

async fn chat(
    endpoint: &Endpoint,
    topic: TopicId,
    ticket: &Ticket,
    name: &str,
    sender: GossipSender,
    receiver: GossipReceiver,
) -> Result<()> {
    let (incoming_tx, mut incoming_rx) = mpsc::channel(16);
    tokio::spawn(receive(receiver, incoming_tx));

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

    let mut app = App::new(topic);
    app.info(format!(
        "Joined as {name}. Ticket to share:\n{ticket}\nLines starting with > are yours."
    ));
    let mut chat = Chat {
        endpoint,
        name,
        sender,
        names: HashMap::new(),
        neighbors: HashSet::new(),
    };
    let result = chat
        .run(&mut terminal, &mut app, &mut incoming_rx, &mut event_rx)
        .await;

    if enhanced_keys {
        execute!(std::io::stdout(), PopKeyboardEnhancementFlags)?;
    }
    ratatui::restore();

    if !chat.neighbors.is_empty() {
        chat.broadcast(&Message::Leave).await.ok();
        // Give gossip a moment to send the message before the endpoint closes.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    result
}

struct Chat<'a> {
    endpoint: &'a Endpoint,
    name: &'a str,
    sender: GossipSender,
    /// Nicknames of everyone we have heard from.
    names: HashMap<EndpointId, String>,
    /// Peers we are directly connected to.
    neighbors: HashSet<EndpointId>,
}

impl Chat<'_> {
    async fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        app: &mut App,
        incoming_rx: &mut mpsc::Receiver<Incoming>,
        event_rx: &mut mpsc::Receiver<Event>,
    ) -> Result<()> {
        // Redraw only when something changed: every write to the terminal
        // clears text selection in some terminals (e.g. Emacs vterm).
        let mut dirty = true;
        loop {
            if dirty {
                terminal.draw(|frame| app.draw(frame))?;
                dirty = false;
            }
            tokio::select! {
                Some(incoming) = incoming_rx.recv() => {
                    self.handle_incoming(app, incoming).await;
                    dirty = true;
                }
                event = event_rx.recv() => match event {
                    Some(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        dirty = true;
                        match app.handle_key(key) {
                            Action::None => {}
                            Action::Quit => return Ok(()),
                            Action::Send(text) => {
                                let message = Message::Text { name: self.name.to_string(), text };
                                match self.broadcast(&message).await {
                                    Ok(()) => {
                                        let Message::Text { text, .. } = message else { unreachable!() };
                                        app.push(Entry::Me(text));
                                        app.clear_input();
                                    }
                                    Err(err) => app.info(format!("Send failed: {err:#}")),
                                }
                            }
                        }
                    }
                    Some(Event::Resize(..)) => dirty = true,
                    Some(_) => {}
                    None => return Ok(()),
                },
            }
        }
    }

    async fn handle_incoming(&mut self, app: &mut App, incoming: Incoming) {
        match incoming {
            Incoming::Message(from, Message::Join { name }) => {
                match self.names.insert(from, name.clone()) {
                    None => app.info(format!("{name} joined")),
                    Some(old) if old != name => app.info(format!("{old} is now known as {name}")),
                    Some(_) => {}
                }
            }
            Incoming::Message(from, Message::Text { name, text }) => {
                self.names.insert(from, name.clone());
                app.push(Entry::Peer { id: from, name, text });
            }
            Incoming::Message(from, Message::Leave) => {
                let name = self
                    .names
                    .remove(&from)
                    .unwrap_or_else(|| from.fmt_short().to_string());
                app.info(format!("{name} left"));
            }
            Incoming::NeighborUp(id) => {
                self.neighbors.insert(id);
                // Introduce ourselves, so that newcomers learn our name.
                let join = Message::Join { name: self.name.to_string() };
                if let Err(err) = self.broadcast(&join).await {
                    app.info(format!("Send failed: {err:#}"));
                }
            }
            Incoming::NeighborDown(id) => {
                self.neighbors.remove(&id);
            }
            Incoming::Lagged => app.info("Missed some messages: the chat fell behind."),
            Incoming::Invalid(err) => app.info(format!("Dropped a message: {err:#}")),
            Incoming::Closed(reason) => app.info(format!("Gossip closed: {reason}")),
        }
        app.neighbors = self.neighbors.len();
    }

    async fn broadcast(&self, message: &Message) -> Result<()> {
        let bytes = proto::sign_and_encode(self.endpoint.secret_key(), message)?;
        if bytes.len() > MAX_MESSAGE_SIZE {
            bail!("message is too long");
        }
        self.sender.broadcast(bytes).await?;
        Ok(())
    }
}
