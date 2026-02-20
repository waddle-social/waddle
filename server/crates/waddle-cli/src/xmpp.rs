// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! XMPP client module for the Waddle TUI.
//!
//! Uses `tokio-xmpp::AsyncClient` directly instead of the `xmpp` crate's
//! `Agent`, giving us full access to raw stanzas including custom payloads
//! (embed elements, MAM results, etc.).

use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_xmpp::AsyncClient;
use tokio_xmpp::Event as TokioXmppEvent;
use tracing::{error, info, warn};
use xmpp_parsers::jid::BareJid;

use crate::config::XmppConfig;
use crate::stanza::{self, RawEmbed, StanzaEvent};

/// Events produced by the XMPP client for the application.
#[derive(Debug, Clone)]
pub enum XmppClientEvent {
    Connected,
    Disconnected,
    Error(String),
    RoomJoined {
        room_jid: BareJid,
    },
    RoomLeft {
        room_jid: BareJid,
    },
    RoomMessage {
        room_jid: BareJid,
        sender_nick: String,
        body: String,
        id: Option<String>,
        embeds: Vec<RawEmbed>,
    },
    ChatMessage {
        from: BareJid,
        body: String,
        id: Option<String>,
        embeds: Vec<RawEmbed>,
    },
    RoomSubject {
        room_jid: BareJid,
        subject: String,
    },
    RosterItem {
        jid: BareJid,
        name: Option<String>,
    },
    ContactPresence {
        jid: BareJid,
        available: bool,
        show: Option<String>,
    },
    MamMessage {
        room_jid: Option<BareJid>,
        sender_nick: Option<String>,
        body: String,
        id: Option<String>,
        embeds: Vec<RawEmbed>,
        timestamp: Option<String>,
    },
    MamFinished {
        complete: bool,
    },
    /// Connection retry state for status bar display.
    RetryScheduled {
        attempt: u32,
        delay_secs: f64,
    },
}

/// Connection retry state with jittered exponential backoff.
#[derive(Debug)]
struct RetryState {
    attempt: u32,
    next_retry_at: Option<Instant>,
}

impl RetryState {
    fn new() -> Self {
        Self {
            attempt: 0,
            next_retry_at: None,
        }
    }

    fn reset(&mut self) {
        self.attempt = 0;
        self.next_retry_at = None;
    }

    /// Calculate next retry delay with jittered exponential backoff.
    /// Base: 1s, max: 30s, jitter: ±25%.
    fn schedule_retry(&mut self) -> Duration {
        self.attempt += 1;
        let base_secs: f64 = (2.0f64).powi(self.attempt as i32 - 1).min(30.0);
        // ±25% jitter
        let jitter = 1.0 + (rand_jitter() * 0.5 - 0.25);
        let delay_secs = (base_secs * jitter).max(0.5);
        let delay = Duration::from_secs_f64(delay_secs);
        self.next_retry_at = Some(Instant::now() + delay);
        delay
    }

    fn seconds_until_retry(&self) -> Option<f64> {
        self.next_retry_at
            .map(|t| t.saturating_duration_since(Instant::now()).as_secs_f64())
    }
}

/// Simple deterministic-ish jitter in [0, 1) using time-based seed.
/// We avoid pulling in a full RNG crate just for this.
fn rand_jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos as f64 % 1000.0) / 1000.0
}

/// XMPP client wrapper using tokio-xmpp directly.
pub struct XmppClient {
    client: AsyncClient<tokio_xmpp::starttls::ServerConfig>,
    jid: BareJid,
    nickname: String,
    muc_domain: String,
    event_tx: mpsc::UnboundedSender<XmppClientEvent>,
    retry: RetryState,
}

impl XmppClient {
    /// Create a new XMPP client with the given configuration.
    pub async fn new(
        config: &XmppConfig,
        event_tx: mpsc::UnboundedSender<XmppClientEvent>,
    ) -> Result<Self> {
        let jid_str = config
            .jid
            .as_ref()
            .context("XMPP JID is required")?;

        let jid = BareJid::from_str(jid_str)
            .with_context(|| format!("Invalid JID: {jid_str}"))?;

        let token = config
            .token
            .as_ref()
            .context("XMPP session token is required")?;

        let nickname = jid
            .node()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "user".to_string());

        let muc_domain = config
            .muc_domain
            .clone()
            .unwrap_or_else(|| format!("muc.{}", jid.domain()));

        info!("Creating XMPP client for {} (MUC: {})", jid, muc_domain);

        // Build server connector
        let server_config = if let Some(ref host) = config.server {
            tokio_xmpp::starttls::ServerConfig::Manual {
                host: host.clone(),
                port: config.port,
            }
        } else {
            tokio_xmpp::starttls::ServerConfig::UseSrv
        };

        let client_config = tokio_xmpp::AsyncConfig {
            jid: xmpp_parsers::jid::Jid::from(jid.clone()),
            password: token.clone(),
            server: server_config,
        };

        let mut client = AsyncClient::new_with_config(client_config);
        client.set_reconnect(true);

        Ok(Self {
            client,
            jid,
            nickname,
            muc_domain,
            event_tx,
            retry: RetryState::new(),
        })
    }

    pub fn jid(&self) -> &BareJid {
        &self.jid
    }

    pub fn nickname(&self) -> &str {
        &self.nickname
    }

    pub fn muc_domain(&self) -> &str {
        &self.muc_domain
    }

    /// Poll for the next XMPP event. Returns false if stream ended.
    pub async fn poll_events(&mut self) -> bool {
        match self.client.next().await {
            Some(TokioXmppEvent::Online { .. }) => {
                info!("XMPP connected");
                self.retry.reset();
                let _ = self.event_tx.send(XmppClientEvent::Connected);

                // Send initial presence
                if let Err(e) = self.client.send_stanza(stanza::build_initial_presence()).await {
                    warn!("Failed to send initial presence: {e}");
                }

                // Request roster
                if let Err(e) = self.client.send_stanza(stanza::build_roster_query()).await {
                    warn!("Failed to send roster query: {e}");
                }

                true
            }
            Some(TokioXmppEvent::Disconnected(err)) => {
                warn!("XMPP disconnected: {err}");
                let _ = self.event_tx.send(XmppClientEvent::Disconnected);

                let delay = self.retry.schedule_retry();
                info!(
                    "Scheduling reconnect attempt {} in {:.1}s",
                    self.retry.attempt,
                    delay.as_secs_f64()
                );
                let _ = self.event_tx.send(XmppClientEvent::RetryScheduled {
                    attempt: self.retry.attempt,
                    delay_secs: delay.as_secs_f64(),
                });

                // tokio-xmpp handles reconnection internally when set_reconnect(true),
                // but we track state for UI display
                true
            }
            Some(TokioXmppEvent::Stanza(elem)) => {
                let stanza_events = stanza::dispatch_stanza(elem);
                for event in stanza_events {
                    self.forward_stanza_event(event);
                }
                true
            }
            None => {
                let _ = self.event_tx.send(XmppClientEvent::Disconnected);
                false
            }
        }
    }

    /// Convert a StanzaEvent to an XmppClientEvent and send it.
    fn forward_stanza_event(&self, event: StanzaEvent) {
        let client_event = match event {
            StanzaEvent::RoomMessage {
                room_jid,
                sender_nick,
                body,
                id,
                embeds,
            } => XmppClientEvent::RoomMessage {
                room_jid,
                sender_nick,
                body,
                id,
                embeds,
            },
            StanzaEvent::ChatMessage {
                from,
                body,
                id,
                embeds,
            } => XmppClientEvent::ChatMessage {
                from,
                body,
                id,
                embeds,
            },
            StanzaEvent::RoomSubject {
                room_jid, subject, ..
            } => XmppClientEvent::RoomSubject { room_jid, subject },
            StanzaEvent::RoomJoined { room_jid } => XmppClientEvent::RoomJoined { room_jid },
            StanzaEvent::RoomLeft { room_jid } => XmppClientEvent::RoomLeft { room_jid },
            StanzaEvent::RosterItem(item) => XmppClientEvent::RosterItem {
                jid: item.jid,
                name: item.name,
            },
            StanzaEvent::ContactPresence {
                jid,
                available,
                show,
                ..
            } => XmppClientEvent::ContactPresence {
                jid,
                available,
                show,
            },
            StanzaEvent::MamMessage {
                room_jid,
                sender_nick,
                body,
                id,
                embeds,
                timestamp,
                ..
            } => XmppClientEvent::MamMessage {
                room_jid,
                sender_nick,
                body,
                id,
                embeds,
                timestamp,
            },
            StanzaEvent::MamFinished { complete } => XmppClientEvent::MamFinished { complete },
            StanzaEvent::UnhandledIq(_) => return,
        };

        let _ = self.event_tx.send(client_event);
    }

    /// Join a MUC room by JID.
    pub async fn join_room_jid(&mut self, room_jid: &BareJid, nickname: Option<&str>) {
        let nick = nickname.unwrap_or(&self.nickname);
        info!("Joining room {} as {}", room_jid, nick);
        let elem = stanza::build_muc_join(room_jid, nick);
        if let Err(e) = self.client.send_stanza(elem).await {
            error!("Failed to send MUC join: {e}");
        }
    }

    /// Leave a MUC room.
    pub async fn leave_room(&mut self, room_jid: &BareJid, nickname: Option<&str>) {
        let nick = nickname.unwrap_or(&self.nickname);
        info!("Leaving room {}", room_jid);
        let elem = stanza::build_muc_leave(room_jid, nick);
        if let Err(e) = self.client.send_stanza(elem).await {
            error!("Failed to send MUC leave: {e}");
        }
    }

    /// Send a message to a MUC room.
    pub async fn send_room_message(&mut self, room_jid: &BareJid, message: &str) {
        let elem = stanza::build_room_message(room_jid, message);
        if let Err(e) = self.client.send_stanza(elem).await {
            error!("Failed to send room message: {e}");
        }
    }

    /// Send a direct chat message.
    pub async fn send_chat_message(&mut self, to: &BareJid, message: &str) {
        let elem = stanza::build_chat_message(to, message);
        if let Err(e) = self.client.send_stanza(elem).await {
            error!("Failed to send chat message: {e}");
        }
    }

    /// Request MAM history for a room (or user archive if `room_jid` is None).
    pub async fn request_mam_history(
        &mut self,
        query_id: &str,
        room_jid: Option<&BareJid>,
        max_results: u32,
    ) {
        info!(
            "Requesting MAM history (query_id={}, to={:?}, max={})",
            query_id, room_jid, max_results
        );
        let elem = stanza::build_mam_query(query_id, room_jid, max_results);
        if let Err(e) = self.client.send_stanza(elem).await {
            error!("Failed to send MAM query: {e}");
        }
    }

    /// Disconnect from the XMPP server.
    pub async fn disconnect(&mut self) {
        info!("Disconnecting XMPP client");
        let _ = self.client.send_end().await;
    }

    /// Get seconds until next retry (for status bar display).
    pub fn retry_countdown(&self) -> Option<f64> {
        self.retry.seconds_until_retry()
    }
}

/// Helper to create a room JID from a channel name and MUC domain.
pub fn make_room_jid(channel_name: &str, muc_domain: &str) -> Result<BareJid> {
    let room_name = channel_name.trim_start_matches('#');
    let jid_str = format!("{}@{}", room_name, muc_domain);
    BareJid::from_str(&jid_str).with_context(|| format!("Invalid room JID: {jid_str}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_room_jid() {
        let jid = make_room_jid("#general", "muc.waddle.social").unwrap();
        assert_eq!(jid.to_string(), "general@muc.waddle.social");
    }

    #[test]
    fn test_make_room_jid_no_hash() {
        let jid = make_room_jid("random", "muc.waddle.social").unwrap();
        assert_eq!(jid.to_string(), "random@muc.waddle.social");
    }

    #[test]
    fn test_retry_state_backoff() {
        let mut retry = RetryState::new();
        assert_eq!(retry.attempt, 0);

        let d1 = retry.schedule_retry();
        assert_eq!(retry.attempt, 1);
        assert!(d1.as_secs_f64() >= 0.5);
        assert!(d1.as_secs_f64() <= 2.0);

        let d2 = retry.schedule_retry();
        assert_eq!(retry.attempt, 2);
        assert!(d2.as_secs_f64() >= 1.0);

        // After many attempts, should cap at ~30s
        for _ in 0..20 {
            retry.schedule_retry();
        }
        let d_cap = retry.schedule_retry();
        assert!(d_cap.as_secs_f64() <= 40.0); // 30 * 1.25 jitter

        retry.reset();
        assert_eq!(retry.attempt, 0);
    }
}
