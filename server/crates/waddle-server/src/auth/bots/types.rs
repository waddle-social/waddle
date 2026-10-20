//! Typed identifiers for user bot accounts and their API tokens.

use std::fmt;

use chrono::{DateTime, Utc};
use jid::{BareJid, DomainPart, NodePart};
use uuid::Uuid;

/// Longest bot name, in ASCII characters.
const BOT_NAME_MAX_LEN: usize = 32;
/// Longest token label, in characters, after trimming.
const BOT_TOKEN_LABEL_MAX_LEN: usize = 64;

/// The `users.kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserKind {
    Human,
    Bot,
}

impl UserKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Bot => "bot",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "human" => Some(Self::Human),
            "bot" => Some(Self::Bot),
            _ => None,
        }
    }
}

/// Why a bot is disabled (`users.disabled_reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledReason {
    OwnerMissing,
    OwnerNotMember,
}

impl DisabledReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OwnerMissing => "owner_missing",
            Self::OwnerNotMember => "owner_not_member",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "owner_missing" => Some(Self::OwnerMissing),
            "owner_not_member" => Some(Self::OwnerNotMember),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BotIdError {
    #[error("bot name must be 1-32 characters of a-z, 0-9, '_' or '-', starting and ending with a letter or digit")]
    InvalidName,
    #[error("the owner's account name cannot own bots")]
    OwnerIneligible,
    #[error("not a bot JID")]
    InvalidJid,
    #[error("token label must be 1-64 characters with no control characters")]
    InvalidLabel,
    #[error("token lifetime must be between 1 and 365 days")]
    InvalidTokenTtl,
    #[error("not a bot token id")]
    InvalidTokenId,
}

/// The owner-chosen part of a bot JID: `^[a-z0-9](?:[a-z0-9_-]{0,30}[a-z0-9])?$`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BotName(String);

impl BotName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for BotName {
    type Error = BotIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let bytes = value.as_bytes();
        let edge = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
        let inner = |byte: &u8| edge(byte) || *byte == b'_' || *byte == b'-';
        let valid = match (bytes.first(), bytes.last()) {
            (Some(first), Some(last)) => {
                bytes.len() <= BOT_NAME_MAX_LEN
                    && edge(first)
                    && edge(last)
                    && bytes.iter().all(inner)
            }
            _ => false,
        };
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(BotIdError::InvalidName)
        }
    }
}

impl fmt::Display for BotName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A bot account JID: `<owner node>+<name>@bots.<xmpp domain>`.
///
/// Owner nodes and bot names both exclude `+`, so the node splits into its
/// owner and name in exactly one way.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BotJid(BareJid);

impl BotJid {
    pub fn new(
        owner: &BareJid,
        name: &BotName,
        bots_domain: &DomainPart,
    ) -> Result<Self, BotIdError> {
        let owner_node = owner.node().ok_or(BotIdError::OwnerIneligible)?;
        if owner_node.as_str().contains('+') {
            return Err(BotIdError::OwnerIneligible);
        }
        let localpart = format!("{}+{}", owner_node.as_str(), name.as_str());
        let node = NodePart::new(&localpart).map_err(|_| BotIdError::InvalidJid)?;
        Ok(Self(BareJid::from_parts(Some(&node), bots_domain)))
    }

    /// Parses a client-supplied bot JID, which must live on `bots_domain`.
    pub fn parse(value: &str, bots_domain: &DomainPart) -> Result<Self, BotIdError> {
        let jid: BareJid = value.parse().map_err(|_| BotIdError::InvalidJid)?;
        if jid.domain().as_str() != bots_domain.as_str() {
            return Err(BotIdError::InvalidJid);
        }
        Self::from_storage(jid)
    }

    /// Wraps a JID read from a `kind = 'bot'` users row. The domain is
    /// trusted; the node must still split into an owner and a valid name.
    pub(crate) fn from_storage(jid: BareJid) -> Result<Self, BotIdError> {
        let node = jid.node().ok_or(BotIdError::InvalidJid)?;
        let (owner, name) = node
            .as_str()
            .split_once('+')
            .ok_or(BotIdError::InvalidJid)?;
        if owner.is_empty() || name.contains('+') {
            return Err(BotIdError::InvalidJid);
        }
        BotName::try_from(name).map_err(|_| BotIdError::InvalidJid)?;
        Ok(Self(jid))
    }

    pub fn as_bare(&self) -> &BareJid {
        &self.0
    }

    pub fn into_bare(self) -> BareJid {
        self.0
    }

    /// The full localpart, `<owner node>+<name>`, stored in
    /// `users.username`, `users.xmpp_localpart` and `users.localpart_key`.
    pub fn localpart(&self) -> &str {
        self.0.node().map(|node| node.as_str()).unwrap_or_default()
    }

    pub fn name(&self) -> BotName {
        let name = self
            .localpart()
            .split_once('+')
            .map(|(_, name)| name)
            .unwrap_or_default();
        BotName(name.to_owned())
    }
}

impl fmt::Display for BotJid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Primary key of a `bot_tokens` row. Not a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BotTokenId(Uuid);

impl BotTokenId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn parse(value: &str) -> Result<Self, BotIdError> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| BotIdError::InvalidTokenId)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for BotTokenId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BotTokenId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Owner-chosen token label: trimmed, 1..=64 characters, no control
/// characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotTokenLabel(String);

impl BotTokenLabel {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for BotTokenLabel {
    type Error = BotIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let trimmed = value.trim();
        let len = trimmed.chars().count();
        if len == 0 || len > BOT_TOKEN_LABEL_MAX_LEN || trimmed.chars().any(char::is_control) {
            return Err(BotIdError::InvalidLabel);
        }
        Ok(Self(trimmed.to_owned()))
    }
}

impl fmt::Display for BotTokenLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A bot account as stored in `users`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotRecord {
    pub jid: BotJid,
    pub owner: BareJid,
    pub name: BotName,
    pub display_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub disabled: Option<(DateTime<Utc>, DisabledReason)>,
}
