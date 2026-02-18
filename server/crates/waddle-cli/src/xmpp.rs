// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! XMPP client module for the Waddle TUI.
//!
//! This module provides a wrapper around the `xmpp` crate to handle:
//! - Connection management with STARTTLS
//! - SASL PLAIN authentication using session tokens
//! - MUC (Multi-User Chat) room operations
//! - Message sending and receiving

use anyhow::{Context, Result};
use std::str::FromStr;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use xmpp::parsers::message::MessageType;
use xmpp::{Agent, BareJid, ClientBuilder, ClientFeature, ClientType, Event as XmppEvent, Jid};

use crate::config::XmppConfig;

/// Events produced by the XMPP client for the application
#[derive(Debug, Clone)]
pub enum XmppClientEvent {
    /// Successfully connected and authenticated
    Connected,
    /// Disconnected from the server
    Disconnected,
    /// Connection error occurred
    Error(String),
    /// Successfully joined a MUC room
    RoomJoined { room_jid: BareJid },
    /// Left a MUC room
    RoomLeft { room_jid: BareJid },
    /// Received a message from a MUC room
    RoomMessage {
        room_jid: BareJid,
        sender_nick: String,
        body: String,
        id: Option<String>,
    },
    /// Received a direct chat message
    ChatMessage {
        from: BareJid,
        body: String,
        id: Option<String>,
    },
}

/// XMPP client wrapper for the Waddle TUI
pub struct XmppClient {
    agent: Agent,
    jid: BareJid,
    nickname: String,
    muc_domain: String,
    event_tx: mpsc::UnboundedSender<XmppClientEvent>,
}

impl XmppClient {
    /// Create a new XMPP client with the given configuration
    ///
    /// # Arguments
    /// * `config` - XMPP configuration with JID, server, and token
    /// * `event_tx` - Channel sender for emitting events to the application
    ///
    /// # Returns
    /// A new XmppClient instance ready to process events
    pub async fn new(
        config: &XmppConfig,
        event_tx: mpsc::UnboundedSender<XmppClientEvent>,
    ) -> Result<Self> {
        let jid_str = config
            .jid
            .as_ref()
            .context("XMPP JID is required in configuration")?;

        let jid = BareJid::from_str(jid_str)
            .with_context(|| format!("Invalid JID format: {}", jid_str))?;

        let token = config
            .token
            .as_ref()
            .context("XMPP session token is required")?;

        // Extract nickname from JID (local part)
        let nickname = jid
            .node()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "user".to_string());

        // Determine MUC domain
        let muc_domain = config
            .muc_domain
            .clone()
            .unwrap_or_else(|| format!("muc.{}", jid.domain()));

        info!(
            "Creating XMPP client for {} (MUC domain: {})",
            jid, muc_domain
        );

        // Build the XMPP agent
        // Note: The xmpp crate uses the password field for SASL PLAIN auth
        // We pass our session token as the "password"
        let agent = ClientBuilder::new(jid.clone(), token)
            .set_client(ClientType::Pc, "waddle-cli")
            .set_website("https://waddle.social")
            .set_default_nick(&nickname)
            .enable_feature(ClientFeature::ContactList)
            .build();

        Ok(Self {
            agent,
            jid,
            nickname,
            muc_domain,
            event_tx,
        })
    }

    /// Get our JID
    pub fn jid(&self) -> &BareJid {
        &self.jid
    }

    /// Get our nickname
    pub fn nickname(&self) -> &str {
        &self.nickname
    }

    /// Get the MUC domain
    pub fn muc_domain(&self) -> &str {
        &self.muc_domain
    }

    /// Process pending XMPP events
    ///
    /// This should be called in a loop to handle incoming events.
    /// Returns true if the client is still connected, false if disconnected.
    pub async fn poll_events(&mut self) -> bool {
        match self.agent.wait_for_events().await {
            Some(events) => {
                for event in events {
                    self.handle_event(event);
                }
                true
            }
            None => {
                // Connection closed
                let _ = self.event_tx.send(XmppClientEvent::Disconnected);
                false
            }
        }
    }

    /// Handle a single XMPP event
    fn handle_event(&mut self, event: XmppEvent) {
        match event {
            XmppEvent::Online => {
                info!("XMPP client connected");
                let _ = self.event_tx.send(XmppClientEvent::Connected);
            }
            XmppEvent::Disconnected => {
                info!("XMPP client disconnected");
                let _ = self.event_tx.send(XmppClientEvent::Disconnected);
            }
            XmppEvent::RoomJoined(room_jid) => {
                info!("Joined room: {}", room_jid);
                let _ = self.event_tx.send(XmppClientEvent::RoomJoined { room_jid });
            }
            XmppEvent::RoomLeft(room_jid) => {
                info!("Left room: {}", room_jid);
                let _ = self.event_tx.send(XmppClientEvent::RoomLeft { room_jid });
            }
            XmppEvent::RoomMessage(id, room_jid, sender_nick, body) => {
                let body_str = body.0.clone();
                debug!(
                    "Room message in {}: {}: {}",
                    room_jid, sender_nick, body_str
                );
                let _ = self.event_tx.send(XmppClientEvent::RoomMessage {
                    room_jid,
                    sender_nick,
                    body: body_str,
                    id,
                });
            }
            XmppEvent::ChatMessage(id, from, body) => {
                let body_str = body.0.clone();
                debug!("Chat message from {}: {}", from, body_str);
                let _ = self.event_tx.send(XmppClientEvent::ChatMessage {
                    from,
                    body: body_str,
                    id,
                });
            }
            XmppEvent::RoomPrivateMessage(id, room_jid, sender_nick, body) => {
                let body_str = body.0.clone();
                debug!(
                    "Private message in {} from {}: {}",
                    room_jid, sender_nick, body_str
                );
                // Treat room private messages as regular chat messages
                let _ = self.event_tx.send(XmppClientEvent::ChatMessage {
                    from: room_jid,
                    body: body_str,
                    id,
                });
            }
            XmppEvent::ContactAdded(item) => {
                debug!("Contact added: {:?}", item);
            }
            XmppEvent::ContactRemoved(item) => {
                debug!("Contact removed: {:?}", item);
            }
            XmppEvent::ContactChanged(item) => {
                debug!("Contact changed: {:?}", item);
            }
            XmppEvent::AvatarRetrieved(jid, _hash) => {
                debug!("Avatar retrieved for: {}", jid);
            }
            XmppEvent::ServiceMessage(_id, from, body) => {
                debug!("Service message from {}: {}", from, body.0);
            }
            XmppEvent::HttpUploadedFile(url) => {
                debug!("File uploaded: {}", url);
            }
            _ => {
                debug!("Unhandled XMPP event");
            }
        }
    }

    /// Join a MUC room
    ///
    /// # Arguments
    /// * `room_name` - The room name (without domain), e.g., "general"
    /// * `nickname` - Optional nickname to use; defaults to our JID's local part
    pub async fn join_room(&mut self, room_name: &str, nickname: Option<&str>) {
        let room_jid = match BareJid::from_str(&format!("{}@{}", room_name, self.muc_domain)) {
            Ok(jid) => jid,
            Err(e) => {
                error!("Invalid room JID for {}: {}", room_name, e);
                return;
            }
        };

        let nick = nickname.unwrap_or(&self.nickname);
        info!("Joining room {} as {}", room_jid, nick);

        self.agent
            .join_room(room_jid, Some(nick.to_string()), None, "en", "")
            .await;
    }

    /// Join a MUC room by full JID
    ///
    /// # Arguments
    /// * `room_jid` - The full room JID, e.g., "general@muc.waddle.social"
    /// * `nickname` - Optional nickname to use; defaults to our JID's local part
    pub async fn join_room_jid(&mut self, room_jid: &BareJid, nickname: Option<&str>) {
        let nick = nickname.unwrap_or(&self.nickname);
        info!("Joining room {} as {}", room_jid, nick);

        self.agent
            .join_room(room_jid.clone(), Some(nick.to_string()), None, "en", "")
            .await;
    }

    /// Leave a MUC room
    ///
    /// # Arguments
    /// * `room_jid` - The room JID to leave
    pub async fn leave_room(&mut self, room_jid: &BareJid) {
        info!("Leaving room {}", room_jid);
        // The xmpp crate doesn't have a direct leave_room method in Agent
        // We need to send unavailable presence
        // For now, we'll just log it
        warn!("leave_room not fully implemented yet for {}", room_jid);
    }

    /// Send a message to a MUC room
    ///
    /// # Arguments
    /// * `room_jid` - The room JID to send to
    /// * `message` - The message body
    pub async fn send_room_message(&mut self, room_jid: &BareJid, message: &str) {
        info!("Sending message to {}: {}", room_jid, message);

        self.agent
            .send_message(
                Jid::Bare(room_jid.clone()),
                MessageType::Groupchat,
                "en",
                message,
            )
            .await;
    }

    /// Send a direct chat message
    ///
    /// # Arguments
    /// * `to` - The recipient's JID
    /// * `message` - The message body
    pub async fn send_chat_message(&mut self, to: &BareJid, message: &str) {
        info!("Sending chat message to {}: {}", to, message);

        self.agent
            .send_message(Jid::Bare(to.clone()), MessageType::Chat, "en", message)
            .await;
    }

    /// Disconnect from the XMPP server
    pub async fn disconnect(&mut self) {
        info!("Disconnecting XMPP client");
        self.agent.disconnect().await;
    }
}

/// Helper function to create a room JID from a channel name and MUC domain
pub fn make_room_jid(channel_name: &str, muc_domain: &str) -> Result<BareJid> {
    // Remove leading # from channel names if present
    let room_name = channel_name.trim_start_matches('#');
    let jid_str = format!("{}@{}", room_name, muc_domain);
    BareJid::from_str(&jid_str).with_context(|| format!("Invalid room JID: {}", jid_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_room_jid() {
        let jid = make_room_jid("#general", "muc.waddle.social").unwrap();
        assert_eq!(jid.to_string(), "general@muc.waddle.social");

        let jid = make_room_jid("random", "muc.waddle.social").unwrap();
        assert_eq!(jid.to_string(), "random@muc.waddle.social");
    }
}
