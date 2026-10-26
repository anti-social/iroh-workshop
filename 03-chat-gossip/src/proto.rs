use std::{fmt, str::FromStr};

use anyhow::{Context, Result};
use bytes::Bytes;
use iroh::{EndpointAddr, PublicKey, SecretKey, Signature};
use iroh_gossip::proto::TopicId;
use serde::{Deserialize, Serialize};

/// Everything needed to join a room: its topic and some peers already in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub topic: TopicId,
    pub peers: Vec<EndpointAddr>,
}

/// Serializes to lowercase base32.
impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let bytes = postcard::to_stdvec(self).expect("postcard::to_stdvec is infallible");
        let mut text = data_encoding::BASE32_NOPAD.encode(&bytes);
        text.make_ascii_lowercase();
        write!(f, "{text}")
    }
}

impl FromStr for Ticket {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let bytes = data_encoding::BASE32_NOPAD
            .decode(s.trim().to_ascii_uppercase().as_bytes())
            .context("invalid ticket encoding")?;
        postcard::from_bytes(&bytes).context("invalid ticket")
    }
}

/// What peers say to each other in a room.
#[derive(Debug, Serialize, Deserialize)]
pub enum Message {
    /// Announces our presence and nickname.
    Join { name: String },
    Text { name: String, text: String },
    Leave,
}

/// A message signed by its author.
///
/// Gossip forwards messages through other peers, so the peer that delivered a message
/// is not necessarily its author. The signature proves who wrote it.
#[derive(Debug, Serialize, Deserialize)]
struct SignedMessage {
    from: PublicKey,
    data: Bytes,
    signature: Signature,
}

pub fn sign_and_encode(secret_key: &SecretKey, message: &Message) -> Result<Bytes> {
    let data: Bytes = postcard::to_stdvec(message)?.into();
    let signed = SignedMessage {
        from: secret_key.public(),
        signature: secret_key.sign(&data),
        data,
    };
    Ok(postcard::to_stdvec(&signed)?.into())
}

pub fn verify_and_decode(bytes: &[u8]) -> Result<(PublicKey, Message)> {
    let signed: SignedMessage = postcard::from_bytes(bytes).context("invalid message")?;
    signed
        .from
        .verify(&signed.data, &signed.signature)
        .context("invalid signature")?;
    let message = postcard::from_bytes(&signed.data).context("invalid message body")?;
    Ok((signed.from, message))
}
