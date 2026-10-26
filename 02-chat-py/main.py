"""One-to-one chat over iroh."""

import argparse
import asyncio
import os
import sys
from pathlib import Path

import iroh
from platformdirs import user_config_path
from rich.text import Text
from textual import work
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.widgets import RichLog, Static, TextArea

ALPN = b"iroh-workshop/chat/0"
# Maximum size of a single chat message in bytes.
MAX_MESSAGE_SIZE = 64 * 1024


def default_key_path() -> Path:
    return user_config_path("iroh-chat") / "secret.key"


def keygen(path: Path, force: bool) -> None:
    if path.exists() and not force:
        sys.exit(f"{path} already exists, use --force to overwrite")
    path.parent.mkdir(parents=True, exist_ok=True)
    key = iroh.SecretKey.generate()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "wb") as file:
        file.write(key.to_bytes())
    print(f"Key saved to {path}")
    print(key.public())


def load_key(path: Path) -> bytes:
    try:
        key = path.read_bytes()
    except OSError as err:
        sys.exit(f"failed to read key from {path}, run `keygen` first: {err}")
    if len(key) != 32:
        sys.exit(f"invalid key file {path}")
    return key


def pubkey(path: Path) -> None:
    print(iroh.SecretKey.from_bytes(load_key(path)).public())


async def bind(key_path: Path) -> iroh.Endpoint:
    options = iroh.EndpointOptions(
        preset=iroh.preset_n0(),
        secret_key=load_key(key_path),
        alpns=[ALPN],
    )
    return await iroh.Endpoint.bind(options)


def connection_status(conn: iroh.Connection) -> Text:
    """Describes the path currently used to send data to the peer."""
    paths = conn.paths()
    path = next((path for path in paths if path.is_selected), None)
    if path is None:
        return Text(" no path", style="bright_black")
    if path.is_ip:
        kind, color = "DIRECT", "green"
    elif path.is_relay:
        kind, color = "RELAYED", "yellow"
    else:
        kind, color = "CUSTOM", "blue"
    return Text.assemble(
        (f" {kind} ", f"bold black on {color}"),
        f" via {path.remote_addr} · rtt {path.rtt_ms} ms · {len(paths) - 1} other path(s)",
    )


class ChatApp(App):
    CSS = """
    RichLog {
        border: round $primary;
        border-title-align: left;
    }
    TextArea {
        height: auto;
        max-height: 10;
        border: round $secondary;
        border-title-align: left;
    }
    #status {
        height: 1;
    }
    """

    BINDINGS = [
        Binding("ctrl+enter,alt+enter,ctrl+j", "send", "Send", priority=True),
        Binding("escape,ctrl+c", "quit", "Quit", priority=True),
    ]

    def __init__(self, conn: iroh.Connection) -> None:
        super().__init__()
        self.conn = conn
        self.connected = True

    def compose(self) -> ComposeResult:
        history = RichLog(wrap=True, markup=False)
        history.border_title = f"chat with {self.conn.remote_id().fmt_short()}"
        yield history
        message = TextArea(show_line_numbers=False, highlight_cursor_line=False)
        message.border_title = "message"
        yield message
        yield Static(Text("connecting...", style="bright_black"), id="status")

    def on_mount(self) -> None:
        self.add_message("*", "Connected. Enter: new line, Ctrl+Enter (or Ctrl+J): send, Esc: quit")
        self.query_one(TextArea).focus()
        self.update_status()
        self.set_interval(1, self.update_status)
        self.receive()
        self.watch_closed()

    def add_message(self, label: str, body: str) -> None:
        style = {"peer": "cyan", "me": "green"}.get(label, "bright_black")
        prefix = f"{label}: "
        indent = " " * len(prefix)
        text = Text()
        for i, line in enumerate(body.split("\n")):
            if i:
                text.append("\n")
            text.append(prefix if i == 0 else indent, style=f"bold {style}")
            text.append(line)
        self.query_one(RichLog).write(text)

    def update_status(self) -> None:
        if self.connected:
            self.query_one("#status", Static).update(connection_status(self.conn))

    @work(exclusive=True, group="receive")
    async def receive(self) -> None:
        # Each message arrives on its own uni stream.
        while True:
            try:
                stream = await self.conn.accept_uni()
                data = await stream.read_to_end(MAX_MESSAGE_SIZE)
            except iroh.IrohError:
                return
            self.add_message("peer", data.decode("utf-8", errors="replace"))

    @work(exclusive=True, group="closed")
    async def watch_closed(self) -> None:
        reason = await self.conn.closed()
        self.connected = False
        self.query_one("#status", Static).update(Text(" DISCONNECTED ", style="bold black on red"))
        self.add_message("*", f"Connection closed: {reason}. Press Esc to quit.")

    async def action_send(self) -> None:
        message = self.query_one(TextArea)
        text = message.text
        if not text.strip():
            return
        if not self.connected:
            self.add_message("*", "Not connected.")
            return
        try:
            await send(self.conn, text)
        except Exception as err:
            self.add_message("*", f"Send failed: {err}")
            return
        self.add_message("me", text)
        message.clear()


async def send(conn: iroh.Connection, text: str) -> None:
    data = text.encode()
    if len(data) > MAX_MESSAGE_SIZE:
        raise ValueError("message is too long")
    stream = await conn.open_uni()
    await stream.write_all(data)
    await stream.finish()


async def accept(endpoint: iroh.Endpoint) -> iroh.Connection:
    while True:
        incoming = await endpoint.accept_next()
        if incoming is None:
            sys.exit("endpoint closed")
        try:
            return await (await incoming.accept()).connect()
        except iroh.IrohError as err:
            print(f"incoming connection failed: {err}", file=sys.stderr)


async def chat(endpoint: iroh.Endpoint, conn: iroh.Connection) -> None:
    try:
        await ChatApp(conn).run_async()
    finally:
        conn.close(0, b"bye")
        await endpoint.close()


async def run(args: argparse.Namespace) -> None:
    endpoint = await bind(args.key)
    if args.command == "listen":
        print(f"Listening. Your endpoint id:\n{endpoint.id()}")
        print("Waiting for a peer to connect...")
        conn = await accept(endpoint)
    else:
        try:
            peer = iroh.EndpointId.from_string(args.peer)
        except iroh.IrohError as err:
            sys.exit(f"invalid endpoint id: {err}")
        print(f"Connecting to {peer}...")
        conn = await endpoint.connect(iroh.EndpointAddr(peer, None, []), ALPN)
    await chat(endpoint, conn)


def main() -> None:
    parser = argparse.ArgumentParser(description="One-to-one chat over iroh")
    parser.add_argument(
        "--key",
        type=Path,
        default=default_key_path(),
        help="path to the secret key file (default: %(default)s)",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    keygen_parser = commands.add_parser(
        "keygen", help="generate and store a new endpoint key, then print its public part"
    )
    keygen_parser.add_argument("--force", action="store_true", help="overwrite an existing key")
    commands.add_parser("pubkey", help="print the public part of the stored key")
    commands.add_parser("listen", help="wait for a peer to connect")
    connect_parser = commands.add_parser("connect", help="connect to a peer by its endpoint id")
    connect_parser.add_argument("peer", help="endpoint id of the peer")
    args = parser.parse_args()

    if args.command == "keygen":
        keygen(args.key, args.force)
        return
    if args.command == "pubkey":
        pubkey(args.key)
        return
    try:
        asyncio.run(run(args))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
