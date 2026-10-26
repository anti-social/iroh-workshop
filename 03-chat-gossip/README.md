# chat-gossip

Group terminal chat over [iroh-gossip](https://github.com/n0-computer/iroh-gossip):
everyone in a room receives every message, even peers they are not connected to directly.
Messages are flooded through the swarm, so each one is signed by its author
(the peer that delivers a message is not necessarily the one who wrote it).

## Usage

```sh
cargo run -- keygen                       # create a key, print your endpoint id
cargo run -- --name alice open            # open a new room, print a ticket
cargo run -- --name bob join <ticket>     # join a room using a ticket
```

Every peer prints its own ticket (the room plus all peers it knows about, including itself),
on start and again on exit. Any of these tickets can be used to join.

The key is stored in `~/.config/iroh-chat/secret.key` by default (the same path as `../01-chat`);
pass `--key <path>` to use a different one. To run several peers on one machine, give each one
its own key:

```sh
cargo run -- --key /tmp/a.key keygen
cargo run -- --key /tmp/a.key --name alice open
```

`--name` defaults to `$USER`.

## Keys

- `Enter`: send
- `Shift+Enter`, `Alt+Enter`, `Ctrl+J`, or `\` followed by `Enter`: new line
  (`Shift+Enter` needs a terminal with the kitty keyboard protocol)
- `PageUp` / `PageDown`: scroll history, `Ctrl+Home` / `Ctrl+End`: jump to top / bottom
- `Esc` / `Ctrl+C`: quit

The screen is redrawn only when something changes, and the mouse is not captured, so text can be
selected with the mouse (including in Emacs vterm).

## Protocol

Gossip ALPN `/iroh-gossip/1`, one topic per room. Each gossip message is a postcard-encoded
`SignedMessage { from, data, signature }`, where `data` is a postcard-encoded
`Message::{Join { name }, Text { name, text }, Leave}`. Messages are limited to 64 KiB.
