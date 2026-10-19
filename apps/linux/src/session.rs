//! One signed-in account's XMPP session.
//!
//! The protocol work is the same `WaddleClient` facade the Android and
//! Apple apps call through UniFFI; Linux links it as Rust. Its events are
//! forwarded to the GTK main loop over a channel. Conversations are kept in
//! `waddle-client-session` stores (ADR-0019), so this module only adapts
//! wire records to the core's inputs; it keeps no client state of its own.
//!
//! Ids follow the web client's rules. In a room, replies, reactions and
//! retractions reference the room's XEP-0359 stanza id, trusted only when
//! its `by` is the room. In a DM they reference the sender's origin id or
//! `@id`. Corrections reference the sender's origin id or `@id` in both.
//!
//! Reconnection here is a stopgap with the ADR-0019 backoff curve. The
//! shared `ConnectionController` replaces it.

use std::sync::Arc;
use std::time::Duration;

use chrono::DateTime;
use jid::{BareJid, Jid};
use waddle_client_session::{
    strip_fallback, Attachment, Encryption, IncomingMessage, ReactionUpdate, ReplyTo,
};
use waddle_xmpp_client_ffi::{
    WaddleArchivedMessage, WaddleCarbonDirection, WaddleChannel, WaddleClient, WaddleClientEvent,
    WaddleConfig, WaddleEncryptedFile, WaddleEncryptedFileHash, WaddleEventListener,
    WaddleFallbackRange, WaddleInboxEntry, WaddleMessage, WaddlePresence, WaddleReplyTarget,
    WaddleSendMessageOutcome, WaddleSendOptions, WaddleSharedFile, WaddleStanzaId,
    WaddleStickerHash, WaddleTopology,
};

use crate::attachments::{self, TransferError};
use crate::auth::store::StoredAccount;
use crate::runtime;

const PAGE_SIZE: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Room,
    Direct,
}

impl Kind {
    fn is_muc(self) -> bool {
        self == Self::Room
    }
}

/// A room or DM in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// Room bare JID, or the DM peer's bare JID.
    pub jid: String,
    pub name: String,
    pub kind: Kind,
    pub autojoin: bool,
}

/// What the UI hears from the session.
pub enum SessionEvent {
    Connected,
    Disconnected,
    AuthenticationFailed,
    Message(Box<WaddleMessage>),
    Inbox(WaddleInboxEntry),
    DeliveryAcked(String),
    DeliveryFailed(String),
    Presence(Box<WaddlePresence>),
    Rejected {
        id: String,
        from: String,
        to: Option<String>,
    },
}

/// Who this account is in conversations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Me {
    pub bare: BareJid,
    /// Room nick and display name.
    pub nick: String,
}

/// A live message placed in its conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrival {
    pub conversation: String,
    pub kind: Kind,
    pub message: IncomingMessage,
    /// XEP-0372 mention of this account or the whole room.
    pub mentions_me: bool,
    /// XEP-0203: the server held it while this client was away. The
    /// inbox's unread count already includes it, and it is not news.
    pub delayed: bool,
}

/// A file the user is sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// An uploaded file: what the message carries, and the core's view of it.
#[derive(Clone)]
pub struct Uploaded {
    pub wire: WaddleSharedFile,
    pub attachment: Attachment,
}

/// Quoting a message in a reply (XEP-0461).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    /// The quoted message's reference id.
    pub id: String,
    pub sender: String,
    /// The quoted message is the user's own.
    pub own: bool,
    pub text: String,
}

struct Forwarder(async_channel::Sender<SessionEvent>);

impl WaddleEventListener for Forwarder {
    fn on_event(&self, event: WaddleClientEvent) {
        let forwarded = match event {
            WaddleClientEvent::Connected => SessionEvent::Connected,
            WaddleClientEvent::Disconnected => SessionEvent::Disconnected,
            WaddleClientEvent::AuthenticationFailed { .. } => SessionEvent::AuthenticationFailed,
            WaddleClientEvent::Message { message } => SessionEvent::Message(Box::new(message)),
            WaddleClientEvent::InboxPush { entry } => SessionEvent::Inbox(entry),
            WaddleClientEvent::DeliveryAcked { stanza_id } => {
                SessionEvent::DeliveryAcked(stanza_id)
            }
            WaddleClientEvent::DeliveryFailed { stanza_id } => {
                SessionEvent::DeliveryFailed(stanza_id)
            }
            WaddleClientEvent::Presence { presence } => SessionEvent::Presence(Box::new(presence)),
            WaddleClientEvent::MessageRejected {
                stanza_id,
                from,
                to,
            } => SessionEvent::Rejected {
                id: stanza_id.to_string(),
                from: from.to_string(),
                to: to.map(|jid| jid.to_string()),
            },
            WaddleClientEvent::Error { description } => {
                tracing::warn!(%description, "xmpp client error");
                return;
            }
            // History arrives through fetched pages only (ADR-0019), and the
            // rest has no Linux surface yet.
            _ => return,
        };
        // The receiver lives as long as the window; a send after it closes
        // has nobody to tell.
        let _ = self.0.send_blocking(forwarded);
    }
}

#[derive(Clone)]
pub struct Session {
    client: Arc<WaddleClient>,
    /// For XEP-0363 uploads and fetching shared files.
    transfers: attachments::Transfers,
    pub me: Me,
}

impl Session {
    /// Start a session and return it with its event stream.
    pub fn start(
        account: &StoredAccount,
        transfers: attachments::Transfers,
    ) -> (Self, async_channel::Receiver<SessionEvent>) {
        let (sender, receiver) = async_channel::unbounded();
        let config = WaddleConfig {
            server_url: account.session.websocket_url.to_string(),
            jid: account.session.jid.to_string(),
            access_token: account.session.session_id.expose().to_string(),
            resource: account.resource.clone(),
            // This client never resumes, so it must not ask the server to
            // park the session (that suppresses push; see #1987).
            resumable: false,
        };
        let client = WaddleClient::new(config, Box::new(Forwarder(sender)));
        let session = Self {
            client,
            transfers,
            me: Me {
                bare: account.session.jid.clone(),
                nick: account.session.username.clone(),
            },
        };
        session.connect();
        (session, receiver)
    }

    pub fn connect(&self) {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.connect().await });
    }

    pub async fn disconnect(&self) {
        let client = Arc::clone(&self.client);
        let _ = runtime::spawn(async move { client.disconnect().await }).await;
    }

    pub async fn rooms(&self) -> Vec<Conversation> {
        let client = Arc::clone(&self.client);
        let topology = runtime::spawn(async move { client.discover_topology().await }).await;
        topology.map(rooms_from).unwrap_or_default()
    }

    /// XEP-0430 inbox: every conversation with its unread count.
    pub async fn inbox(&self) -> Vec<WaddleInboxEntry> {
        let client = Arc::clone(&self.client);
        match runtime::spawn(async move { client.fetch_inbox(false, true).await }).await {
            Ok(Ok(inbox)) => inbox.conversations,
            Ok(Err(error)) => {
                tracing::warn!(?error, "could not fetch the inbox");
                Vec::new()
            }
            Err(_) => Vec::new(),
        }
    }

    pub fn mark_read(&self, jid: &str) {
        let client = Arc::clone(&self.client);
        let jid = jid.to_string();
        runtime::spawn(async move {
            if let Err(error) = client.mark_inbox_read(jid, None).await {
                tracing::warn!(?error, "could not mark the conversation read");
            }
        });
    }

    /// RFC 6121 initial presence: without it the server treats this
    /// resource as unavailable and does not deliver 1:1 messages to it.
    pub fn announce(&self) {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.send_presence(None, None, None).await });
    }

    pub fn join(&self, room: &Conversation) {
        let client = Arc::clone(&self.client);
        let (jid, nick) = (room.jid.clone(), self.me.nick.clone());
        runtime::spawn(async move { client.join_room(jid, nick).await });
    }

    /// The newest page before `before` (or the newest page), oldest first.
    pub async fn history(
        &self,
        jid: &str,
        kind: Kind,
        before: Option<String>,
    ) -> (Vec<IncomingMessage>, Option<String>, bool) {
        let client = Arc::clone(&self.client);
        let peer = jid.to_string();
        let page = runtime::spawn(async move {
            match kind {
                Kind::Room => client.fetch_room_history(peer, PAGE_SIZE, before).await,
                Kind::Direct => client.fetch_dm_history(peer, PAGE_SIZE, before).await,
            }
        })
        .await;
        let (Ok(page), Ok(conversation)) = (page, jid.parse::<BareJid>()) else {
            return (Vec::new(), None, false);
        };
        let messages = page
            .messages
            .into_iter()
            .filter_map(|message| archived_from(message, &conversation, kind, &self.me))
            .collect();
        (messages, page.first_id, page.is_complete)
    }

    /// Send under `origin_id`, optionally as a reply and with uploaded
    /// files. True when the stanza was written.
    pub async fn send(
        &self,
        jid: &str,
        kind: Kind,
        body: String,
        origin_id: String,
        quote: Option<Quote>,
        files: Vec<WaddleSharedFile>,
    ) -> bool {
        let mut options = WaddleSendOptions {
            stanza_id: Some(origin_id),
            shared_files: files,
            ..WaddleSendOptions::default()
        };
        let mut body = body;
        if let Some(quote) = quote {
            let (quoted, start, end) =
                waddle_client_session::with_reply_fallback(&quote.sender, &quote.text, &body);
            body = quoted;
            options.fallback = Some(WaddleFallbackRange { start, end });
            options.reply = Some(WaddleReplyTarget {
                author_jid: reply_author(kind, jid, &quote, &self.me),
                message_id: quote.id,
            });
        }
        let client = Arc::clone(&self.client);
        let peer = jid.to_string();
        let outcome = runtime::spawn(async move {
            match kind {
                Kind::Room => {
                    client
                        .send_groupchat_message(peer, body, Some(options))
                        .await
                }
                Kind::Direct => client.send_chat_message(peer, body, Some(options)).await,
            }
        })
        .await;
        matches!(outcome, Ok(WaddleSendMessageOutcome::Sent { .. }))
    }

    /// Encrypt and upload a file as the web client does (XEP-0448 over
    /// XEP-0363), ready to share in a message (XEP-0447).
    pub async fn upload(&self, file: Outgoing) -> Result<Uploaded, TransferError> {
        let client = Arc::clone(&self.client);
        let transfers = self.transfers.clone();
        runtime::spawn(async move {
            let sealed = attachments::seal(&file.bytes)?;
            let size =
                u64::try_from(sealed.ciphertext.len()).map_err(|_| TransferError::TooLarge)?;
            let service = client
                .discover_upload_service()
                .await
                .ok_or(TransferError::NoUploadService)?;
            let slot = client
                .request_upload_slot(
                    service,
                    attachments::upload_name(&file.name),
                    size,
                    "application/octet-stream".to_string(),
                )
                .await
                .ok_or(TransferError::SlotRefused)?;
            // The GET address must be fetchable by this client too.
            transfers.check(&slot.get_url)?;
            let headers: Vec<(String, String)> = slot
                .put_headers
                .iter()
                .map(|header| (header.name.clone(), header.value.clone()))
                .collect();
            transfers
                .put(&slot.put_url, &headers, sealed.ciphertext.clone())
                .await?;
            Ok(shared_file(&file, &sealed, slot.get_url))
        })
        .await
        .map_err(|_| TransferError::Interrupted)?
    }

    /// Fetch a shared file, check it, and decrypt it if it is encrypted.
    pub async fn fetch(&self, attachment: Attachment) -> Result<Vec<u8>, TransferError> {
        let transfers = self.transfers.clone();
        runtime::spawn(async move {
            let sources = match &attachment.encryption {
                Some(encryption) => encryption.sources.clone(),
                None => vec![attachment.url.clone()],
            };
            let mut last = TransferError::InsecureUrl;
            for source in sources {
                match transfers.fetch(&source).await {
                    Ok(bytes) => {
                        return match &attachment.encryption {
                            Some(encryption) => Ok(attachments::open(
                                &bytes,
                                &encryption.cipher,
                                &encryption.key_b64,
                                &encryption.iv_b64,
                                &encryption.sha256_b64,
                            )?),
                            None => Ok(bytes),
                        };
                    }
                    Err(error) => last = error,
                }
            }
            Err(last)
        })
        .await
        .map_err(|_| TransferError::Interrupted)?
    }

    /// XEP-0444: replace this account's reactions on `target`.
    pub async fn react(&self, jid: &str, kind: Kind, target: String, emojis: Vec<String>) -> bool {
        let client = Arc::clone(&self.client);
        let peer = jid.to_string();
        runtime::spawn(async move {
            client
                .send_reaction(peer, target, emojis, kind.is_muc())
                .await
        })
        .await
        .unwrap_or(false)
    }

    /// XEP-0308: replace the body of the message with `correction_id`.
    pub async fn correct(
        &self,
        jid: &str,
        kind: Kind,
        correction_id: String,
        body: String,
    ) -> bool {
        let client = Arc::clone(&self.client);
        let peer = jid.to_string();
        let outcome = runtime::spawn(async move {
            client
                .send_correction(peer, correction_id, body, kind.is_muc(), None)
                .await
        })
        .await;
        matches!(outcome, Ok(WaddleSendMessageOutcome::Sent { .. }))
    }

    /// XEP-0424: retract the message with `reference_id`.
    pub async fn retract(&self, jid: &str, kind: Kind, reference_id: String) -> bool {
        let client = Arc::clone(&self.client);
        let peer = jid.to_string();
        runtime::spawn(async move {
            client
                .send_retraction(peer, reference_id, kind.is_muc())
                .await
        })
        .await
        .unwrap_or(false)
    }
}

/// The XEP-0447 file for an upload, and the core's view of it.
fn shared_file(file: &Outgoing, sealed: &attachments::Sealed, get_url: String) -> Uploaded {
    let wire = WaddleSharedFile {
        url: get_url.clone(),
        name: Some(file.name.clone()),
        media_type: Some(file.media_type.clone()),
        size: u64::try_from(file.bytes.len()).ok(),
        width: None,
        height: None,
        desc: None,
        hashes: vec![WaddleStickerHash {
            algo: attachments::SHA_256.to_string(),
            value_b64: sealed.plaintext_sha256_b64.clone(),
        }],
        disposition: attachments::disposition(&file.media_type).to_string(),
        encrypted: Some(WaddleEncryptedFile {
            cipher: attachments::AES_256_GCM.to_string(),
            key_b64: sealed.key_b64.clone(),
            iv_b64: sealed.iv_b64.clone(),
            hashes: vec![WaddleEncryptedFileHash {
                algo: attachments::SHA_256.to_string(),
                value_b64: sealed.ciphertext_sha256_b64.clone(),
            }],
            sources: vec![get_url],
        }),
    };
    let attachment = attachment_from(&wire);
    Uploaded { wire, attachment }
}

/// The core's view of a shared file.
pub fn attachment_from(file: &WaddleSharedFile) -> Attachment {
    Attachment {
        url: file.url.clone(),
        name: file.name.clone(),
        media_type: file.media_type.clone(),
        size: file.size,
        inline: file.disposition == "inline",
        encryption: file.encrypted.as_ref().map(|encrypted| Encryption {
            cipher: encrypted.cipher.clone(),
            key_b64: encrypted.key_b64.clone(),
            iv_b64: encrypted.iv_b64.clone(),
            sha256_b64: encrypted
                .hashes
                .iter()
                .filter(|hash| hash.algo.eq_ignore_ascii_case(attachments::SHA_256))
                .map(|hash| hash.value_b64.clone())
                .collect(),
            sources: if encrypted.sources.is_empty() {
                vec![file.url.clone()]
            } else {
                encrypted.sources.clone()
            },
        }),
    }
}

/// XEP-0461 `to`: the quoted message's author. In a room, its occupant
/// JID; in a DM, whichever side wrote it.
fn reply_author(kind: Kind, conversation: &str, quote: &Quote, me: &Me) -> String {
    match kind {
        Kind::Room => format!("{conversation}/{}", quote.sender),
        Kind::Direct if quote.own => me.bare.to_string(),
        Kind::Direct => conversation.to_string(),
    }
}

/// Reconnect delay for the `attempt`th retry: uniform in
/// [1 s, min(60 s, 2ⁿ s)] (ADR-0019), from the caller's randomness.
pub fn reconnect_delay(attempt: u32, unit_random: f64) -> Duration {
    let ceiling = 2f64
        .powi(i32::try_from(attempt.min(6)).unwrap_or(6))
        .min(60.0);
    let seconds = 1.0 + (ceiling - 1.0).max(0.0) * unit_random.clamp(0.0, 1.0);
    Duration::from_secs_f64(seconds)
}

/// Rooms for the sidebar: channels, not group DMs, in catalogue order.
pub fn rooms_from(topology: WaddleTopology) -> Vec<Conversation> {
    let mut channels: Vec<WaddleChannel> = topology
        .channels
        .into_iter()
        .filter(|channel| !channel.is_group_dm)
        .collect();
    channels.sort_by(|a, b| {
        (a.space_id.as_str(), a.position, a.name.as_str()).cmp(&(
            b.space_id.as_str(),
            b.position,
            b.name.as_str(),
        ))
    });
    channels
        .into_iter()
        .map(|channel| Conversation {
            name: channel.bookmark_name.unwrap_or(channel.name),
            jid: channel.room_jid,
            kind: Kind::Room,
            autojoin: channel.autojoin,
        })
        .collect()
}

/// DMs for the sidebar from the inbox, most recent first.
pub fn direct_from(inbox: &[WaddleInboxEntry]) -> Vec<Conversation> {
    let mut entries: Vec<&WaddleInboxEntry> = inbox
        .iter()
        .filter(|entry| entry.kind == "direct" && entry.thread_id.is_none())
        .collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.last_updated.unwrap_or(0)));
    entries
        .into_iter()
        .filter_map(|entry| direct_conversation(&entry.partner))
        .collect()
}

/// A DM conversation with this peer, named by its local part.
pub fn direct_conversation(peer: &str) -> Option<Conversation> {
    let bare: BareJid = peer.parse().ok()?;
    Some(Conversation {
        name: display_name(&bare),
        jid: bare.to_string(),
        kind: Kind::Direct,
        autojoin: false,
    })
}

fn display_name(jid: &BareJid) -> String {
    jid.node()
        .map(|node| node.as_str().to_string())
        .unwrap_or_else(|| jid.to_string())
}

/// Place a live message in its room or DM.
pub fn arrival(message: WaddleMessage, me: &Me) -> Option<Arrival> {
    let from: Jid = message.from.as_deref()?.parse().ok()?;
    match message.message_type.as_str() {
        "groupchat" => room_arrival(message, from, me),
        "chat" if !message.muc_user && !message.is_muc => direct_arrival(message, from, me),
        _ => None,
    }
}

fn room_arrival(message: WaddleMessage, from: Jid, me: &Me) -> Option<Arrival> {
    let room = from.to_bare();
    let delayed = message.timestamp.is_some();
    let Some(nick) = from
        .resource()
        .map(|resource| resource.as_str().to_string())
    else {
        // The room itself: only its XEP-0425 moderation broadcasts matter.
        let target = message.moderation_target_id?;
        return Some(Arrival {
            conversation: room.to_string(),
            kind: Kind::Room,
            message: IncomingMessage {
                moderates_id: Some(target),
                timestamp_ms: timestamp_ms(message.timestamp.as_deref()),
                ..IncomingMessage::default()
            },
            mentions_me: false,
            delayed,
        });
    };
    let stanza_id = verified_stanza_id(&message.stanza_ids, &room);
    let mentions_me = message.broadcast_mention.is_some()
        || message
            .mention_uris
            .iter()
            .any(|uri| uri.strip_prefix("xmpp:") == Some(me.bare.as_str()));
    let own = nick == me.nick;
    let incoming = IncomingMessage {
        reference_id: stanza_id.clone(),
        stanza_id,
        origin_id: message.origin_id,
        message_id: message.id,
        own,
        body: reply_body(
            message.body,
            message.reply_fallback_start,
            message.reply_fallback_end,
        ),
        timestamp_ms: timestamp_ms(message.timestamp.as_deref()),
        replaces_id: message.replaces_id,
        // A retraction relayed by the room on a moderator's behalf comes
        // from the room itself (above), never from an occupant.
        retracts_id: message.retracts_id,
        moderates_id: None,
        tombstone: message.is_retracted,
        reply_to: message.reply_to_id.map(|id| ReplyTo {
            id,
            sender: message.reply_to_sender.as_deref().and_then(occupant_nick),
        }),
        reaction: reaction(message.reaction_target_id, message.reaction_emojis),
        attachments: message.shared_files.iter().map(attachment_from).collect(),
        sender: nick,
        // A live room message does not expose the occupant's real JID.
        author_id: None,
    };
    Some(Arrival {
        conversation: room.to_string(),
        kind: Kind::Room,
        message: incoming,
        mentions_me: mentions_me && !own,
        delayed,
    })
}

fn direct_arrival(message: WaddleMessage, from: Jid, me: &Me) -> Option<Arrival> {
    let delayed = message.timestamp.is_some();
    let from_me = from.to_bare() == me.bare;
    let own = from_me || message.carbon == Some(WaddleCarbonDirection::Sent);
    let peer = if own {
        message.to.as_deref()?.parse::<Jid>().ok()?.to_bare()
    } else {
        from.to_bare()
    };
    if peer == me.bare {
        return None;
    }
    let reference_id = message.origin_id.clone().or_else(|| message.id.clone());
    let incoming = IncomingMessage {
        stanza_id: verified_stanza_id(&message.stanza_ids, &me.bare),
        origin_id: message.origin_id,
        message_id: message.id,
        reference_id,
        sender: if own {
            me.nick.clone()
        } else {
            display_name(&peer)
        },
        author_id: Some(if own {
            me.bare.to_string()
        } else {
            peer.to_string()
        }),
        own,
        body: reply_body(
            message.body,
            message.reply_fallback_start,
            message.reply_fallback_end,
        ),
        timestamp_ms: timestamp_ms(message.timestamp.as_deref()),
        replaces_id: message.replaces_id,
        retracts_id: message.retracts_id,
        moderates_id: None,
        tombstone: message.is_retracted,
        reply_to: message.reply_to_id.map(|id| ReplyTo { id, sender: None }),
        reaction: reaction(message.reaction_target_id, message.reaction_emojis),
        attachments: message.shared_files.iter().map(attachment_from).collect(),
    };
    Some(Arrival {
        conversation: peer.to_string(),
        kind: Kind::Direct,
        message: incoming,
        mentions_me: false,
        delayed,
    })
}

/// An archived message of `conversation` in the core's terms.
pub fn archived_from(
    message: WaddleArchivedMessage,
    conversation: &BareJid,
    kind: Kind,
    me: &Me,
) -> Option<IncomingMessage> {
    let from: Jid = message.from.as_deref()?.parse().ok()?;
    let timestamp = timestamp_ms(message.timestamp.as_deref());
    let (sender, own, reference_id, author_id) = match kind {
        Kind::Room => {
            let Some(nick) = from
                .resource()
                .map(|resource| resource.as_str().to_string())
            else {
                let target = message.moderation_target_id?;
                return Some(IncomingMessage {
                    moderates_id: Some(target),
                    timestamp_ms: timestamp,
                    ..IncomingMessage::default()
                });
            };
            // The real JID, when the archive exposes it, outlasts the nick.
            let author_id = message
                .author_real_jid
                .as_deref()
                .and_then(|real| real.parse::<Jid>().ok())
                .map(|real| real.to_bare().to_string());
            let own = match author_id.as_deref() {
                Some(real) => real == me.bare.as_str(),
                None => nick == me.nick,
            };
            // The room archive's id is the room's stanza id (XEP-0313).
            (nick, own, Some(message.mam_id.clone()), author_id)
        }
        Kind::Direct => {
            let own = from.to_bare() == me.bare;
            let sender = if own {
                me.nick.clone()
            } else {
                display_name(conversation)
            };
            let reference = message.origin_id.clone().or_else(|| message.id.clone());
            (sender, own, reference, Some(from.to_bare().to_string()))
        }
    };
    Some(IncomingMessage {
        stanza_id: Some(message.mam_id),
        origin_id: message.origin_id,
        message_id: message.id,
        reference_id,
        sender,
        author_id,
        own,
        body: reply_body(
            message.body,
            message.reply_fallback_start,
            message.reply_fallback_end,
        ),
        timestamp_ms: timestamp,
        replaces_id: message.replaces_id,
        retracts_id: message.retracts_id,
        moderates_id: None,
        tombstone: message.is_retracted,
        reply_to: message.reply_to_id.map(|id| ReplyTo {
            id,
            sender: match kind {
                Kind::Room => message.reply_to_sender.as_deref().and_then(occupant_nick),
                Kind::Direct => None,
            },
        }),
        reaction: reaction(message.reaction_target_id, message.reaction_emojis),
        attachments: message.shared_files.iter().map(attachment_from).collect(),
    })
}

/// The stanza id `by` the expected archive (XEP-0359 §Security
/// Considerations): anyone can put a `<stanza-id/>` on what they send.
fn verified_stanza_id(ids: &[WaddleStanzaId], authority: &BareJid) -> Option<String> {
    ids.iter()
        .find(|id| id.by.parse::<BareJid>().is_ok_and(|by| &by == authority))
        .map(|id| id.id.clone())
}

fn reaction(target: Option<String>, emojis: Vec<String>) -> Option<ReactionUpdate> {
    target.map(|target_id| ReactionUpdate { target_id, emojis })
}

fn reply_body(body: Option<String>, start: Option<u32>, end: Option<u32>) -> Option<String> {
    let body = body?;
    Some(match (start, end) {
        (Some(start), Some(end)) => strip_fallback(&body, start, end),
        _ => body,
    })
}

/// `room@muc.example/nick` → `nick`.
fn occupant_nick(full: &str) -> Option<String> {
    let jid: Jid = full.parse().ok()?;
    jid.resource().map(|resource| resource.as_str().to_string())
}

fn timestamp_ms(raw: Option<&str>) -> i64 {
    raw.and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|at| at.timestamp_millis())
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Me {
        Me {
            bare: "david@waddle.social".parse().expect("jid"),
            nick: "david".into(),
        }
    }

    fn channel(name: &str, position: i32, group_dm: bool) -> WaddleChannel {
        WaddleChannel {
            id: name.into(),
            room_jid: format!("{name}@muc.waddle.social"),
            name: name.into(),
            description: None,
            channel_type: "text".into(),
            position,
            space_id: "default".into(),
            autojoin: true,
            bookmark_name: None,
            is_group_dm: group_dm,
        }
    }

    fn stanza_id(id: &str, by: &str) -> WaddleStanzaId {
        WaddleStanzaId {
            id: id.into(),
            by: by.into(),
        }
    }

    fn wire(from: &str, kind: &str, body: &str) -> WaddleMessage {
        WaddleMessage {
            id: Some("m1".into()),
            from: Some(from.into()),
            to: Some("david@waddle.social/linux".into()),
            body: Some(body.into()),
            subject: None,
            message_type: kind.into(),
            timestamp: Some("2026-10-18T12:00:00Z".into()),
            stanza_id: None,
            stanza_id_by: None,
            stanza_ids: Vec::new(),
            origin_id: Some("o1".into()),
            replaces_id: None,
            retracts_id: None,
            retraction_id: None,
            is_retracted: false,
            moderation_target_id: None,
            moderated_by: None,
            moderation_reason: None,
            reaction_target_id: None,
            reaction_emojis: Vec::new(),
            chat_state: None,
            displayed_marker_requested: false,
            displayed_marker_id: None,
            is_muc: kind == "groupchat",
            muc_user: false,
            thread: None,
            parent_thread_id: None,
            markup_spans: Vec::new(),
            broadcast_mention: None,
            mention_uris: Vec::new(),
            references: Vec::new(),
            forum_post_kind: None,
            forum_title: None,
            is_sticker: false,
            link_previews: Vec::new(),
            pin_event: None,
            call_thread_ended: None,
            safety_scores: None,
            carbon: None,
            reply_to_id: None,
            reply_to_sender: None,
            reply_fallback_start: None,
            reply_fallback_end: None,
            call_thread: None,
            shared_files: Vec::new(),
            mds_displayed: None,
        }
    }

    #[test]
    fn rooms_are_channels_in_catalogue_order() {
        let rooms = rooms_from(WaddleTopology {
            spaces: Vec::new(),
            channels: vec![
                channel("random", 2, false),
                channel("dm-ann-bob", 0, true),
                channel("general", 1, false),
            ],
        });
        let names: Vec<&str> = rooms.iter().map(|room| room.name.as_str()).collect();
        assert_eq!(names, ["general", "random"]);
        assert_eq!(rooms[0].jid, "general@muc.waddle.social");
    }

    #[test]
    fn room_messages_trust_only_the_rooms_stanza_id() {
        let mut message = wire("general@muc.waddle.social/ann", "groupchat", "hi");
        message.stanza_ids = vec![
            stanza_id("forged", "ann@waddle.social"),
            stanza_id("s1", "general@muc.waddle.social"),
        ];
        let arrival = arrival(message.clone(), &me()).expect("a room message");
        assert_eq!(arrival.conversation, "general@muc.waddle.social");
        assert_eq!(arrival.kind, Kind::Room);
        assert_eq!(arrival.message.stanza_id.as_deref(), Some("s1"));
        assert_eq!(arrival.message.reference_id.as_deref(), Some("s1"));
        assert_eq!(arrival.message.sender, "ann");
        assert!(!arrival.message.own);

        message.stanza_ids = vec![stanza_id("forged", "ann@waddle.social")];
        let arrival = super::arrival(message, &me()).expect("a room message");
        assert_eq!(arrival.message.stanza_id, None);
        assert_eq!(arrival.message.reference_id, None);
    }

    #[test]
    fn only_the_room_itself_can_moderate() {
        let mut from_room = wire("general@muc.waddle.social", "groupchat", "");
        from_room.moderation_target_id = Some("s1".into());
        let arrival = arrival(from_room, &me()).expect("a moderation");
        assert_eq!(arrival.message.moderates_id.as_deref(), Some("s1"));

        let mut from_occupant = wire("general@muc.waddle.social/mallory", "groupchat", "x");
        from_occupant.moderation_target_id = Some("s1".into());
        let arrival = super::arrival(from_occupant, &me()).expect("a room message");
        assert_eq!(arrival.message.moderates_id, None);
    }

    #[test]
    fn mentions_of_me_or_the_room_count_but_not_my_own() {
        let mut message = wire("general@muc.waddle.social/ann", "groupchat", "@david hi");
        message.mention_uris = vec!["xmpp:david@waddle.social".into()];
        assert!(arrival(message, &me()).expect("arrival").mentions_me);

        let mut everyone = wire("general@muc.waddle.social/ann", "groupchat", "@here");
        everyone.broadcast_mention = Some("here".into());
        assert!(arrival(everyone, &me()).expect("arrival").mentions_me);

        let mut own = wire("general@muc.waddle.social/david", "groupchat", "@here");
        own.broadcast_mention = Some("here".into());
        assert!(!arrival(own, &me()).expect("arrival").mentions_me);
    }

    #[test]
    fn direct_messages_land_with_the_peer_both_ways() {
        let incoming =
            arrival(wire("ann@waddle.social/phone", "chat", "hey"), &me()).expect("a DM");
        assert_eq!(incoming.conversation, "ann@waddle.social");
        assert_eq!(incoming.kind, Kind::Direct);
        assert_eq!(incoming.message.sender, "ann");
        assert_eq!(incoming.message.reference_id.as_deref(), Some("o1"));

        // A carbon of a send from another of my devices.
        let mut sent = wire("david@waddle.social/desktop", "chat", "hey back");
        sent.to = Some("ann@waddle.social".into());
        sent.carbon = Some(WaddleCarbonDirection::Sent);
        let sent = arrival(sent, &me()).expect("a DM");
        assert_eq!(sent.conversation, "ann@waddle.social");
        assert!(sent.message.own);
        assert_eq!(sent.message.sender, "david");

        // Room private messages are room traffic, not DMs.
        let mut private = wire("general@muc.waddle.social/ann", "chat", "psst");
        private.muc_user = true;
        assert_eq!(arrival(private, &me()), None);
    }

    #[test]
    fn messages_held_while_away_are_marked_delayed() {
        let held = wire("ann@waddle.social/phone", "chat", "while you were out");
        assert!(arrival(held, &me()).expect("a DM").delayed);
        let mut live = wire("ann@waddle.social/phone", "chat", "now");
        live.timestamp = None;
        assert!(!arrival(live, &me()).expect("a DM").delayed);
    }

    #[test]
    fn replies_lose_their_fallback_quote() {
        let mut message = wire(
            "general@muc.waddle.social/ann",
            "groupchat",
            "> bob wrote:\n> hi\nhello",
        );
        message.reply_to_id = Some("s0".into());
        message.reply_to_sender = Some("general@muc.waddle.social/bob".into());
        message.reply_fallback_start = Some(0);
        message.reply_fallback_end = Some(18);
        let arrival = arrival(message, &me()).expect("arrival");
        assert_eq!(arrival.message.body.as_deref(), Some("hello"));
        assert_eq!(
            arrival.message.reply_to,
            Some(ReplyTo {
                id: "s0".into(),
                sender: Some("bob".into())
            })
        );
    }

    #[test]
    fn dm_conversations_come_from_the_inbox_newest_first() {
        let entry = |partner: &str, kind: &str, updated: i64| WaddleInboxEntry {
            partner: partner.into(),
            kind: kind.into(),
            last_stanza_id: None,
            last_updated: Some(updated),
            unread: 0,
            preview: None,
            thread_id: None,
            thread_title: None,
            reply_count: None,
            author: None,
        };
        let direct = direct_from(&[
            entry("ann@waddle.social", "direct", 1),
            entry("general@muc.waddle.social", "muc", 9),
            entry("bob@waddle.social", "direct", 5),
        ]);
        let names: Vec<&str> = direct.iter().map(|dm| dm.name.as_str()).collect();
        assert_eq!(names, ["bob", "ann"]);
    }

    #[test]
    fn replies_name_the_quoted_author() {
        let quote = |own: bool, sender: &str| Quote {
            id: "q".into(),
            sender: sender.into(),
            own,
            text: "hi".into(),
        };
        let room = "general@muc.waddle.social";
        assert_eq!(
            reply_author(Kind::Room, room, &quote(false, "ann"), &me()),
            "general@muc.waddle.social/ann"
        );
        let peer = "ann@waddle.social";
        assert_eq!(
            reply_author(Kind::Direct, peer, &quote(false, "ann"), &me()),
            peer
        );
        assert_eq!(
            reply_author(Kind::Direct, peer, &quote(true, "david"), &me()),
            "david@waddle.social"
        );
    }

    #[test]
    fn archived_room_messages_carry_the_real_author_when_exposed() {
        let archived = |real: Option<&str>| WaddleArchivedMessage {
            mam_id: "s1".into(),
            query_id: None,
            id: Some("m1".into()),
            stanza_id: None,
            stanza_id_by: None,
            stanza_ids: Vec::new(),
            origin_id: Some("o1".into()),
            timestamp: Some("2026-10-18T12:00:00Z".into()),
            from: Some("general@muc.waddle.social/david".into()),
            to: None,
            message_type: "groupchat".into(),
            body: Some("hi".into()),
            subject: None,
            replaces_id: None,
            retracts_id: None,
            retraction_id: None,
            is_retracted: false,
            moderation_target_id: None,
            moderated_by: None,
            moderation_reason: None,
            reaction_target_id: None,
            reaction_emojis: Vec::new(),
            thread: None,
            parent_thread_id: None,
            reply_to_id: None,
            reply_to_sender: None,
            reply_fallback_start: None,
            reply_fallback_end: None,
            markup_spans: Vec::new(),
            broadcast_mention: None,
            mention_uris: Vec::new(),
            references: Vec::new(),
            forum_post_kind: None,
            forum_title: None,
            is_sticker: false,
            muc_user: false,
            author_real_jid: real.map(str::to_string),
            call_thread: None,
            call_thread_ended: None,
            safety_scores: None,
            shared_files: Vec::new(),
            link_previews: Vec::new(),
            call_event: None,
        };
        let room: BareJid = "general@muc.waddle.social".parse().expect("jid");
        // Someone else now holding the nick "david" is not the user.
        let other = archived_from(
            archived(Some("mallory@waddle.social/x")),
            &room,
            Kind::Room,
            &me(),
        )
        .expect("a message");
        assert!(!other.own);
        assert_eq!(other.author_id.as_deref(), Some("mallory@waddle.social"));
        assert_eq!(other.reference_id.as_deref(), Some("s1"));

        let mine = archived_from(
            archived(Some("david@waddle.social/linux")),
            &room,
            Kind::Room,
            &me(),
        )
        .expect("a message");
        assert!(mine.own);
        let anonymous = archived_from(archived(None), &room, Kind::Room, &me()).expect("a message");
        assert!(anonymous.own, "falls back to the nick");
        assert_eq!(anonymous.author_id, None);
    }

    #[test]
    fn timestamps_parse_rfc3339_to_utc_millis() {
        assert_eq!(timestamp_ms(Some("1970-01-01T00:00:01+00:00")), 1000);
        assert_eq!(timestamp_ms(Some("1970-01-01T01:00:01+01:00")), 1000);
    }

    #[test]
    fn reconnect_backoff_follows_the_adr_curve() {
        assert_eq!(reconnect_delay(0, 0.9), Duration::from_secs(1));
        assert_eq!(reconnect_delay(3, 0.0), Duration::from_secs(1));
        assert_eq!(reconnect_delay(3, 1.0), Duration::from_secs(8));
        assert_eq!(reconnect_delay(40, 1.0), Duration::from_secs(60));
    }
}
