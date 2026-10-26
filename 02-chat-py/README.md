# chat (Python)

One-to-one terminal chat over [iroh](https://iroh.computer), built with the
`iroh` Python bindings and [Textual](https://textual.textualize.io).
It speaks the same protocol as `../01-chat`, so the two can talk to each other:
ALPN `iroh-workshop/chat/0`, one unidirectional stream per message, each holding
the plain UTF-8 text of that message.

## Usage

```sh
uv run main.py keygen            # create a key, print your endpoint id
uv run main.py pubkey            # print the endpoint id of the stored key
uv run main.py listen            # wait for a peer
uv run main.py connect <peer-id> # connect to a peer
```

The key is stored in `~/.config/iroh-chat/secret.key` by default (the same path as
the Rust version uses); pass `--key <path>` to use a different one.

## Keys

- `Enter`: new line
- `Ctrl+Enter`: send. This needs a terminal with the kitty keyboard protocol
  (kitty, WezTerm, foot, Ghostty, recent Alacritty/iTerm2). In other terminals use `Ctrl+J`.
- `Esc` / `Ctrl+C`: quit

The bottom line shows whether the peer is reached `DIRECT` (with its IP address and port)
or `RELAYED` (with the relay URL), plus the round-trip time.
