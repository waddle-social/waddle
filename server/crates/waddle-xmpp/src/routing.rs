//! Stanza routing for XMPP federation.
//!
//! This module provides the `StanzaRouter` which determines whether a JID is local
//! or remote (requires S2S federation) and routes stanzas accordingly.
//!
//! # Routing Logic
//!
//! For each stanza, the router:
//! 1. Extracts the destination JID from the stanza
//! 2. Checks if the JID's domain matches the local domain
//! 3. If local: routes via the `ConnectionRegistry` to local users
//! 4. If remote: routes via the `S2sConnectionPool` to the remote server
//!
//! # Example
//!
//! ```ignore
//! use waddle_xmpp::routing::StanzaRouter;
//!
//! let router = StanzaRouter::new(
//!     "waddle.social".to_string(),
//!     connection_registry,
//!     Some(s2s_pool),
//! );
//!
//! // Route a message - automatically determines local vs remote
//! router.route_message(message, sender_jid).await?;
//! ```

use std::sync::Arc;

use jid::{BareJid, FullJid, Jid};
use tracing::{debug, info, instrument, warn};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;
use xmpp_parsers::presence::Presence;

use crate::connection::Stanza;
use crate::muc::MucRoomRegistry;
use crate::registry::{ConnectionRegistry, SendResult};
use crate::s2s::pool::{S2sConnectionPool, S2sPoolError};
use crate::XmppError;

/// Result of a routing operation.
#[derive(Debug)]
pub enum RoutingResult {
    /// Stanza was delivered successfully to local user(s)
    DeliveredLocal {
        /// Number of recipients that received the stanza
        delivered_count: usize,
        /// Number of recipients that were offline
        offline_count: usize,
    },
    /// Stanza was sent to remote server via S2S
    SentToRemote {
        /// The remote domain the stanza was sent to
        domain: String,
    },
    /// No destination JID in stanza
    NoDestination,
    /// S2S federation is not enabled
    FederationDisabled,
    /// Routing failed
    Failed {
        /// Error description
        reason: String,
    },
}

/// Configuration for the stanza router.
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// The local domain for this server
    pub local_domain: String,
    /// The MUC subdomain (e.g., "muc.waddle.social")
    pub muc_domain: String,
    /// The Spaces subdomain (e.g., "spaces.waddle.social") for XEP-0503
    pub spaces_domain: String,
    /// The SFU subdomain (e.g., "sfu.waddle.social")
    pub sfu_domain: String,
    /// Whether S2S federation is enabled
    pub federation_enabled: bool,
}

impl RouterConfig {
    /// Create a new router configuration.
    pub fn new(local_domain: String) -> Self {
        let muc_domain = format!("muc.{}", local_domain);
        let spaces_domain = format!("spaces.{}", local_domain);
        let sfu_domain = format!("sfu.{}", local_domain);
        Self {
            local_domain,
            muc_domain,
            spaces_domain,
            sfu_domain,
            federation_enabled: false,
        }
    }

    /// Enable S2S federation.
    pub fn with_federation(mut self, enabled: bool) -> Self {
        self.federation_enabled = enabled;
        self
    }

    /// Set a custom MUC domain.
    pub fn with_muc_domain(mut self, muc_domain: String) -> Self {
        self.muc_domain = muc_domain;
        self
    }
}

/// Determines the routing destination for a JID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingDestination {
    /// JID is local to this server
    Local,
    /// JID is for the MUC service on this server
    LocalMuc,
    /// JID is for the Spaces service on this server (XEP-0503)
    LocalSpaces,
    /// JID is for the SFU service on this server
    LocalSfu,
    /// JID is on a remote server (requires S2S)
    Remote {
        /// The remote domain
        domain: String,
    },
}

/// Stanza router for local and S2S message delivery.
///
/// The router examines each stanza's destination JID and determines whether
/// to deliver locally via the connection registry or remotely via S2S federation.
pub struct StanzaRouter {
    /// Router configuration
    config: RouterConfig,
    /// Connection registry for local users
    connection_registry: Arc<ConnectionRegistry>,
    /// MUC room registry for Muji/Jingle participant validation.
    muc_room_registry: Option<Arc<MucRoomRegistry>>,
    /// S2S connection pool for remote servers (None if federation disabled)
    s2s_pool: Option<Arc<S2sConnectionPool>>,
}

impl StanzaRouter {
    /// Create a new stanza router.
    ///
    /// # Arguments
    ///
    /// * `config` - Router configuration including local domain
    /// * `connection_registry` - Registry of local connections for message delivery
    /// * `s2s_pool` - Optional S2S connection pool for federation (None = federation disabled)
    pub fn new(
        config: RouterConfig,
        connection_registry: Arc<ConnectionRegistry>,
        s2s_pool: Option<Arc<S2sConnectionPool>>,
    ) -> Self {
        let federation_enabled = s2s_pool.is_some() && config.federation_enabled;

        info!(
            local_domain = %config.local_domain,
            muc_domain = %config.muc_domain,
            federation_enabled = federation_enabled,
            "StanzaRouter initialized"
        );

        Self {
            config,
            connection_registry,
            muc_room_registry: None,
            s2s_pool,
        }
    }

    /// Attach a MUC room registry for Muji/Jingle routing validation.
    pub fn with_muc_room_registry(mut self, registry: Arc<MucRoomRegistry>) -> Self {
        self.muc_room_registry = Some(registry);
        self
    }

    /// Get the router configuration.
    pub fn config(&self) -> &RouterConfig {
        &self.config
    }

    /// Get the local domain.
    pub fn local_domain(&self) -> &str {
        &self.config.local_domain
    }

    /// Check if S2S federation is enabled.
    pub fn is_federation_enabled(&self) -> bool {
        self.s2s_pool.is_some() && self.config.federation_enabled
    }

    /// Determine the routing destination for a JID.
    ///
    /// Returns whether the JID is local, local MUC, or remote.
    pub fn get_destination(&self, jid: &Jid) -> RoutingDestination {
        let domain = jid.domain().as_str();
        self.get_destination_for_domain(domain)
    }

    /// Determine the routing destination for a domain string.
    pub fn get_destination_for_domain(&self, domain: &str) -> RoutingDestination {
        if domain == self.config.local_domain {
            RoutingDestination::Local
        } else if domain == self.config.muc_domain {
            RoutingDestination::LocalMuc
        } else if domain == self.config.spaces_domain {
            RoutingDestination::LocalSpaces
        } else if domain == self.config.sfu_domain {
            RoutingDestination::LocalSfu
        } else {
            RoutingDestination::Remote {
                domain: domain.to_string(),
            }
        }
    }

    /// Check if a JID is local to this server.
    pub fn is_local_jid(&self, jid: &Jid) -> bool {
        matches!(
            self.get_destination(jid),
            RoutingDestination::Local
                | RoutingDestination::LocalMuc
                | RoutingDestination::LocalSpaces
                | RoutingDestination::LocalSfu
        )
    }

    /// Check if a JID is for the local MUC service.
    pub fn is_muc_jid(&self, jid: &Jid) -> bool {
        matches!(self.get_destination(jid), RoutingDestination::LocalMuc)
    }

    /// Check if a JID is for the local SFU service.
    pub fn is_sfu_jid(&self, jid: &Jid) -> bool {
        jid.domain().as_str() == self.config.sfu_domain
    }

    /// Return the SFU domain for this server.
    pub fn sfu_domain(&self) -> &str {
        &self.config.sfu_domain
    }

    /// Check if a JID requires S2S federation.
    pub fn is_remote_jid(&self, jid: &Jid) -> bool {
        matches!(self.get_destination(jid), RoutingDestination::Remote { .. })
    }

    /// Route a message stanza to its destination.
    ///
    /// For local recipients, the message is sent via the connection registry.
    /// For remote recipients, the message is sent via S2S federation.
    #[instrument(skip(self, message), fields(to = ?message.to, msg_type = ?message.type_))]
    pub async fn route_message(
        &self,
        message: Message,
        _sender_jid: &FullJid,
    ) -> Result<RoutingResult, XmppError> {
        let to_jid = match &message.to {
            Some(jid) => jid,
            None => {
                debug!("Message has no destination JID");
                return Ok(RoutingResult::NoDestination);
            }
        };

        match self.get_destination(to_jid) {
            RoutingDestination::Local => self.route_message_local(message).await,
            RoutingDestination::LocalMuc
            | RoutingDestination::LocalSpaces
            | RoutingDestination::LocalSfu => {
                // MUC/Spaces/SFU messages should be handled by their respective services,
                // not by this router directly. Return as local.
                debug!("Message to local service should be handled by service handler");
                Ok(RoutingResult::DeliveredLocal {
                    delivered_count: 0,
                    offline_count: 0,
                })
            }
            RoutingDestination::Remote { domain } => {
                self.route_message_remote(message, &domain).await
            }
        }
    }

    /// Route a message to local users.
    ///
    /// This is also used by the S2S listener to route inbound messages
    /// from remote servers to local recipients.
    pub async fn route_message_local(&self, message: Message) -> Result<RoutingResult, XmppError> {
        let to_jid = message.to.as_ref().ok_or_else(|| {
            XmppError::bad_request(Some("Message has no destination".to_string()))
        })?;

        // Get the bare JID for looking up all resources
        let bare_jid: BareJid = match to_jid.clone().try_into_full() {
            Ok(full) => full.to_bare(),
            Err(bare) => bare,
        };

        // Get all connected resources for this user
        let resources = self.connection_registry.get_resources_for_user(&bare_jid);

        if resources.is_empty() {
            debug!(to = %bare_jid, "Recipient has no connected resources");
            return Ok(RoutingResult::DeliveredLocal {
                delivered_count: 0,
                offline_count: 1,
            });
        }

        let stanza = Stanza::Message(message);
        let mut delivered_count = 0;
        let mut offline_count = 0;

        // Send to all connected resources
        for resource_jid in &resources {
            match self
                .connection_registry
                .send_to(resource_jid, stanza.clone())
                .await
            {
                SendResult::Sent => {
                    debug!(to = %resource_jid, "Message delivered to local user");
                    delivered_count += 1;
                }
                SendResult::NotConnected | SendResult::ChannelClosed => {
                    debug!(to = %resource_jid, "Local user not connected");
                    offline_count += 1;
                }
                SendResult::ChannelFull => {
                    warn!(to = %resource_jid, "Channel full, message dropped");
                    offline_count += 1;
                }
            }
        }

        Ok(RoutingResult::DeliveredLocal {
            delivered_count,
            offline_count,
        })
    }

    /// Route a message to a remote server via S2S.
    async fn route_message_remote(
        &self,
        message: Message,
        domain: &str,
    ) -> Result<RoutingResult, XmppError> {
        if !self.is_federation_enabled() {
            debug!(domain = %domain, "S2S federation disabled, cannot route to remote");
            return Ok(RoutingResult::FederationDisabled);
        }

        let pool = self
            .s2s_pool
            .as_ref()
            .ok_or_else(|| XmppError::internal("S2S pool not available".to_string()))?;

        // Serialize the message to XML
        let xml = message_to_xml(&message)?;

        // Send the stanza through the S2S connection pool
        match pool.send_stanza(domain, xml.as_bytes()).await {
            Ok(()) => {
                info!(
                    domain = %domain,
                    "Message sent to remote server via S2S"
                );

                Ok(RoutingResult::SentToRemote {
                    domain: domain.to_string(),
                })
            }
            Err(S2sPoolError::Shutdown) => {
                Err(XmppError::internal("S2S pool is shutting down".to_string()))
            }
            Err(e) => {
                warn!(domain = %domain, error = %e, "Failed to send message via S2S");
                Ok(RoutingResult::Failed {
                    reason: format!("S2S send failed: {}", e),
                })
            }
        }
    }

    /// Route a presence stanza to its destination.
    #[instrument(skip(self, presence), fields(to = ?presence.to, presence_type = ?presence.type_))]
    pub async fn route_presence(
        &self,
        presence: Presence,
        _sender_jid: &FullJid,
    ) -> Result<RoutingResult, XmppError> {
        let to_jid = match &presence.to {
            Some(jid) => jid,
            None => {
                // Presence without 'to' is a broadcast - not routed here
                debug!("Presence has no destination JID (broadcast)");
                return Ok(RoutingResult::NoDestination);
            }
        };

        match self.get_destination(to_jid) {
            RoutingDestination::Local => self.route_presence_local(presence).await,
            RoutingDestination::LocalMuc
            | RoutingDestination::LocalSpaces
            | RoutingDestination::LocalSfu => {
                // MUC/Spaces/SFU presence should be handled by their respective services
                debug!("Presence to local service should be handled by service handler");
                Ok(RoutingResult::DeliveredLocal {
                    delivered_count: 0,
                    offline_count: 0,
                })
            }
            RoutingDestination::Remote { domain } => {
                self.route_presence_remote(presence, &domain).await
            }
        }
    }

    /// Route presence to local users.
    ///
    /// This is also used by the S2S listener to route inbound presence
    /// from remote servers to local recipients.
    pub async fn route_presence_local(
        &self,
        presence: Presence,
    ) -> Result<RoutingResult, XmppError> {
        // Clone the destination JID before moving presence into the stanza
        let to_jid = presence.to.clone().ok_or_else(|| {
            XmppError::bad_request(Some("Presence has no destination".to_string()))
        })?;

        // For presence, we usually send to a specific full JID
        let stanza = Stanza::Presence(presence);

        match to_jid.try_into_full() {
            Ok(full_jid) => {
                // Send to specific resource
                match self.connection_registry.send_to(&full_jid, stanza).await {
                    SendResult::Sent => Ok(RoutingResult::DeliveredLocal {
                        delivered_count: 1,
                        offline_count: 0,
                    }),
                    _ => Ok(RoutingResult::DeliveredLocal {
                        delivered_count: 0,
                        offline_count: 1,
                    }),
                }
            }
            Err(bare_jid) => {
                // Send to all resources
                let resources = self.connection_registry.get_resources_for_user(&bare_jid);
                let mut delivered = 0;
                let mut offline = 0;

                for resource_jid in resources {
                    match self
                        .connection_registry
                        .send_to(&resource_jid, stanza.clone())
                        .await
                    {
                        SendResult::Sent => delivered += 1,
                        _ => offline += 1,
                    }
                }

                Ok(RoutingResult::DeliveredLocal {
                    delivered_count: delivered,
                    offline_count: offline,
                })
            }
        }
    }

    /// Route presence to a remote server via S2S.
    async fn route_presence_remote(
        &self,
        presence: Presence,
        domain: &str,
    ) -> Result<RoutingResult, XmppError> {
        if !self.is_federation_enabled() {
            return Ok(RoutingResult::FederationDisabled);
        }

        let pool = self
            .s2s_pool
            .as_ref()
            .ok_or_else(|| XmppError::internal("S2S pool not available".to_string()))?;

        // Serialize the presence to XML
        let xml = presence_to_xml(&presence)?;

        // Send the stanza through the S2S connection pool
        match pool.send_stanza(domain, xml.as_bytes()).await {
            Ok(()) => {
                info!(
                    domain = %domain,
                    "Presence sent to remote server via S2S"
                );

                Ok(RoutingResult::SentToRemote {
                    domain: domain.to_string(),
                })
            }
            Err(S2sPoolError::Shutdown) => {
                Err(XmppError::internal("S2S pool is shutting down".to_string()))
            }
            Err(e) => {
                warn!(domain = %domain, error = %e, "Failed to send presence via S2S");
                Ok(RoutingResult::Failed {
                    reason: format!("S2S send failed: {}", e),
                })
            }
        }
    }

    /// Route an IQ stanza to its destination.
    #[instrument(skip(self, iq), fields(to = ?iq.to))]
    pub async fn route_iq(&self, iq: Iq, sender_jid: &FullJid) -> Result<RoutingResult, XmppError> {
        let to_jid = match &iq.to {
            Some(jid) => jid,
            None => {
                // IQ without 'to' is directed at the server
                debug!("IQ has no destination JID (server query)");
                return Ok(RoutingResult::NoDestination);
            }
        };

        match self.get_destination(to_jid) {
            RoutingDestination::Local => self.route_iq_local(iq).await,
            RoutingDestination::LocalMuc => self.route_iq_local_muc(iq, Some(sender_jid)).await,
            RoutingDestination::LocalSpaces => {
                // MUC/Spaces IQs should be handled by their respective services
                debug!("IQ to local service should be handled by service handler");
                Ok(RoutingResult::DeliveredLocal {
                    delivered_count: 0,
                    offline_count: 0,
                })
            }
            RoutingDestination::LocalSfu => {
                debug!("IQ to local SFU service — not yet wired");
                Ok(RoutingResult::DeliveredLocal {
                    delivered_count: 0,
                    offline_count: 0,
                })
            }
            RoutingDestination::Remote { domain } => self.route_iq_remote(iq, &domain).await,
        }
    }

    /// Route IQ to local users.
    ///
    /// This is also used by the S2S listener to route inbound IQs
    /// from remote servers to local recipients.
    pub async fn route_iq_local(&self, iq: Iq) -> Result<RoutingResult, XmppError> {
        // Clone the destination JID before moving iq into the stanza
        let to_jid = iq
            .to
            .clone()
            .ok_or_else(|| XmppError::bad_request(Some("IQ has no destination".to_string())))?;

        if self.is_muc_jid(&to_jid) {
            let sender = iq
                .from
                .as_ref()
                .and_then(|jid| jid.clone().try_into_full().ok());
            self.validate_muc_jingle_iq(&iq, sender.as_ref()).await?;
        }

        self.route_iq_local_unchecked(iq, to_jid).await
    }

    async fn route_iq_local_unchecked(
        &self,
        iq: Iq,
        to_jid: Jid,
    ) -> Result<RoutingResult, XmppError> {
        let stanza = Stanza::Iq(iq);

        match to_jid.try_into_full() {
            Ok(full_jid) => match self.connection_registry.send_to(&full_jid, stanza).await {
                SendResult::Sent => Ok(RoutingResult::DeliveredLocal {
                    delivered_count: 1,
                    offline_count: 0,
                }),
                _ => Ok(RoutingResult::DeliveredLocal {
                    delivered_count: 0,
                    offline_count: 1,
                }),
            },
            Err(bare_jid) => {
                // For bare JID, send to first available resource
                let resources = self.connection_registry.get_resources_for_user(&bare_jid);

                if let Some(resource_jid) = resources.first() {
                    match self.connection_registry.send_to(resource_jid, stanza).await {
                        SendResult::Sent => Ok(RoutingResult::DeliveredLocal {
                            delivered_count: 1,
                            offline_count: 0,
                        }),
                        _ => Ok(RoutingResult::DeliveredLocal {
                            delivered_count: 0,
                            offline_count: 1,
                        }),
                    }
                } else {
                    Ok(RoutingResult::DeliveredLocal {
                        delivered_count: 0,
                        offline_count: 1,
                    })
                }
            }
        }
    }

    /// Route IQs addressed to the local MUC domain.
    ///
    /// For safety, Jingle IQs addressed to a bare room JID are rejected because
    /// bare-JID fan-out would otherwise deliver to an arbitrary occupant resource.
    async fn route_iq_local_muc(
        &self,
        iq: Iq,
        sender_jid: Option<&FullJid>,
    ) -> Result<RoutingResult, XmppError> {
        let to_jid = iq
            .to
            .clone()
            .ok_or_else(|| XmppError::bad_request(Some("IQ has no destination".to_string())))?;
        self.validate_muc_jingle_iq(&iq, sender_jid).await?;
        self.route_iq_local_unchecked(iq, to_jid).await
    }

    async fn validate_muc_jingle_iq(
        &self,
        iq: &Iq,
        sender_jid: Option<&FullJid>,
    ) -> Result<(), XmppError> {
        if !crate::xep::is_jingle_iq(iq) {
            return Ok(());
        }

        let to_jid = iq
            .to
            .as_ref()
            .ok_or_else(|| XmppError::bad_request(Some("IQ has no destination".to_string())))?;

        let to_full = to_jid.clone().try_into_full().map_err(|_| {
            XmppError::bad_request(Some(
                "Jingle IQ to local MUC must target a full JID".to_string(),
            ))
        })?;
        let room_jid = to_full.to_bare();
        let target_nick = to_full.resource().as_str();

        let registry = self.muc_room_registry.as_ref().ok_or_else(|| {
            XmppError::service_unavailable(Some(
                "MUC registry unavailable for Jingle routing validation".to_string(),
            ))
        })?;
        let room_data = registry.get_room_data(&room_jid).ok_or_else(|| {
            XmppError::item_not_found(Some(format!("MUC room {} not found", room_jid)))
        })?;
        let room = room_data.read().await;

        if room.get_occupant(target_nick).is_none() {
            return Err(XmppError::item_not_found(Some(format!(
                "Target occupant '{}' not found in room",
                target_nick
            ))));
        }

        let effective_sender = iq
            .from
            .as_ref()
            .and_then(|jid| jid.clone().try_into_full().ok())
            .or_else(|| sender_jid.cloned())
            .ok_or_else(|| {
                XmppError::bad_request(Some(
                    "Jingle IQ to local MUC requires a full sender JID".to_string(),
                ))
            })?;

        let sender_in_room = if effective_sender.to_bare() == room_jid {
            room.get_occupant(effective_sender.resource().as_str())
                .is_some()
        } else {
            room.find_occupant_by_real_jid(&effective_sender).is_some()
        };

        if !sender_in_room {
            return Err(XmppError::forbidden(Some(
                "Sender is not an occupant of target room".to_string(),
            )));
        }

        Ok(())
    }

    /// Route IQ to a remote server via S2S.
    async fn route_iq_remote(&self, iq: Iq, domain: &str) -> Result<RoutingResult, XmppError> {
        if !self.is_federation_enabled() {
            return Ok(RoutingResult::FederationDisabled);
        }

        let pool = self
            .s2s_pool
            .as_ref()
            .ok_or_else(|| XmppError::internal("S2S pool not available".to_string()))?;

        // Serialize the IQ to XML
        let xml = iq_to_xml(&iq)?;

        // Send the stanza through the S2S connection pool
        match pool.send_stanza(domain, xml.as_bytes()).await {
            Ok(()) => {
                info!(
                    domain = %domain,
                    "IQ sent to remote server via S2S"
                );

                Ok(RoutingResult::SentToRemote {
                    domain: domain.to_string(),
                })
            }
            Err(S2sPoolError::Shutdown) => {
                Err(XmppError::internal("S2S pool is shutting down".to_string()))
            }
            Err(e) => {
                warn!(domain = %domain, error = %e, "Failed to send IQ via S2S");
                Ok(RoutingResult::Failed {
                    reason: format!("S2S send failed: {}", e),
                })
            }
        }
    }
}

/// Convert a Message to XML string.
fn message_to_xml(message: &Message) -> Result<String, XmppError> {
    use minidom::Element;
    let element: Element = message.clone().into();
    Ok(String::from(&element))
}

/// Convert a Presence to XML string.
fn presence_to_xml(presence: &Presence) -> Result<String, XmppError> {
    use minidom::Element;
    let element: Element = presence.clone().into();
    Ok(String::from(&element))
}

/// Convert an IQ to XML string.
fn iq_to_xml(iq: &Iq) -> Result<String, XmppError> {
    use minidom::Element;
    let element: Element = iq.clone().into();
    Ok(String::from(&element))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::muc::{MucRoomRegistry, Occupant, RoomConfig};
    use crate::types::{Affiliation, Role};
    use minidom::Element;
    use tokio::sync::mpsc;

    fn create_test_config() -> RouterConfig {
        RouterConfig::new("waddle.social".to_string())
    }

    fn create_test_jid(jid_str: &str) -> Jid {
        jid_str.parse().unwrap()
    }

    fn parse_iq(xml: &str) -> Iq {
        let elem: Element = xml.parse().expect("valid xml");
        Iq::try_from(elem).expect("valid iq")
    }

    async fn create_router_with_test_room() -> (StanzaRouter, Arc<ConnectionRegistry>) {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let muc_registry = Arc::new(MucRoomRegistry::new("muc.waddle.social".to_string()));

        let room_jid: BareJid = "room@muc.waddle.social".parse().unwrap();
        muc_registry
            .create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                RoomConfig::default(),
            )
            .unwrap();

        let room_data = muc_registry.get_room_data(&room_jid).expect("room data");
        let mut room = room_data.write().await;
        room.add_occupant(Occupant {
            real_jid: "sender@waddle.social/resource".parse().unwrap(),
            nick: "sender-nick".to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: false,
            home_server: None,
        });
        room.add_occupant(Occupant {
            real_jid: "target@waddle.social/resource".parse().unwrap(),
            nick: "target-nick".to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: false,
            home_server: None,
        });
        drop(room);

        let router = StanzaRouter::new(config, Arc::clone(&registry), None)
            .with_muc_room_registry(muc_registry);
        (router, registry)
    }

    #[test]
    fn test_router_config() {
        let config = RouterConfig::new("example.com".to_string());
        assert_eq!(config.local_domain, "example.com");
        assert_eq!(config.muc_domain, "muc.example.com");
        assert!(!config.federation_enabled);

        let config = config.with_federation(true);
        assert!(config.federation_enabled);

        let config = config.with_muc_domain("chat.example.com".to_string());
        assert_eq!(config.muc_domain, "chat.example.com");
    }

    #[test]
    fn test_get_destination_local() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        let jid = create_test_jid("user@waddle.social");
        assert_eq!(router.get_destination(&jid), RoutingDestination::Local);

        let jid = create_test_jid("user@waddle.social/resource");
        assert_eq!(router.get_destination(&jid), RoutingDestination::Local);
    }

    #[test]
    fn test_get_destination_muc() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        let jid = create_test_jid("room@muc.waddle.social");
        assert_eq!(router.get_destination(&jid), RoutingDestination::LocalMuc);

        let jid = create_test_jid("room@muc.waddle.social/nick");
        assert_eq!(router.get_destination(&jid), RoutingDestination::LocalMuc);
    }

    #[test]
    fn test_get_destination_spaces() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        let jid = create_test_jid("spaces.waddle.social");
        assert_eq!(
            router.get_destination(&jid),
            RoutingDestination::LocalSpaces
        );
    }

    #[test]
    fn test_get_destination_remote() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        let jid = create_test_jid("user@example.com");
        assert_eq!(
            router.get_destination(&jid),
            RoutingDestination::Remote {
                domain: "example.com".to_string()
            }
        );

        let jid = create_test_jid("user@other.social/resource");
        assert_eq!(
            router.get_destination(&jid),
            RoutingDestination::Remote {
                domain: "other.social".to_string()
            }
        );
    }

    #[test]
    fn test_is_local_jid() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        assert!(router.is_local_jid(&create_test_jid("user@waddle.social")));
        assert!(router.is_local_jid(&create_test_jid("room@muc.waddle.social")));
        assert!(router.is_local_jid(&create_test_jid("spaces.waddle.social")));
        assert!(!router.is_local_jid(&create_test_jid("user@example.com")));
    }

    #[test]
    fn test_is_muc_jid() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        assert!(!router.is_muc_jid(&create_test_jid("user@waddle.social")));
        assert!(router.is_muc_jid(&create_test_jid("room@muc.waddle.social")));
        assert!(!router.is_muc_jid(&create_test_jid("user@example.com")));
    }

    #[test]
    fn test_is_remote_jid() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        assert!(!router.is_remote_jid(&create_test_jid("user@waddle.social")));
        assert!(!router.is_remote_jid(&create_test_jid("room@muc.waddle.social")));
        assert!(router.is_remote_jid(&create_test_jid("user@example.com")));
    }

    #[test]
    fn test_federation_disabled_by_default() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        assert!(!router.is_federation_enabled());
    }

    #[tokio::test]
    async fn test_route_message_local_not_connected() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        let sender_jid: FullJid = "sender@waddle.social/resource".parse().unwrap();
        let mut message = Message::new(Some(Jid::from(
            "user@waddle.social".parse::<BareJid>().unwrap(),
        )));
        message.id = Some("test-123".to_string());

        let result = router.route_message(message, &sender_jid).await.unwrap();

        match result {
            RoutingResult::DeliveredLocal {
                delivered_count,
                offline_count,
            } => {
                assert_eq!(delivered_count, 0);
                assert_eq!(offline_count, 1);
            }
            _ => panic!("Expected DeliveredLocal result"),
        }
    }

    #[tokio::test]
    async fn test_route_message_no_destination() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        let sender_jid: FullJid = "sender@waddle.social/resource".parse().unwrap();
        let message = Message::new(None);

        let result = router.route_message(message, &sender_jid).await.unwrap();

        assert!(matches!(result, RoutingResult::NoDestination));
    }

    #[tokio::test]
    async fn test_route_message_remote_federation_disabled() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        let sender_jid: FullJid = "sender@waddle.social/resource".parse().unwrap();
        let message = Message::new(Some(Jid::from(
            "user@example.com".parse::<BareJid>().unwrap(),
        )));

        let result = router.route_message(message, &sender_jid).await.unwrap();

        assert!(matches!(result, RoutingResult::FederationDisabled));
    }

    #[tokio::test]
    async fn test_route_iq_local_muc_bare_non_jingle_routes_to_connected_resource() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let (tx, mut rx) = mpsc::channel(16);
        let full_room_jid: FullJid = "room@muc.waddle.social/nick".parse().unwrap();
        registry.register(full_room_jid, tx);
        let router = StanzaRouter::new(config, registry, None);

        let sender_jid: FullJid = "sender@waddle.social/resource".parse().unwrap();
        let iq = parse_iq(
            r#"<iq xmlns='jabber:client' type='get' from='sender@waddle.social/resource' to='room@muc.waddle.social' id='iq-muc-get'>
                <query xmlns='jabber:iq:version'/>
            </iq>"#,
        );

        let result = router.route_iq(iq, &sender_jid).await.unwrap();
        assert!(matches!(
            result,
            RoutingResult::DeliveredLocal {
                delivered_count: 1,
                offline_count: 0
            }
        ));
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn test_route_iq_local_muc_rejects_bare_jid_jingle() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);

        let sender_jid: FullJid = "sender@waddle.social/resource".parse().unwrap();
        let iq = parse_iq(
            r#"<iq xmlns='jabber:client' type='set' from='sender@waddle.social/resource' to='room@muc.waddle.social' id='iq-jingle-1'>
                <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='abc123'/>
            </iq>"#,
        );

        let result = router.route_iq(iq, &sender_jid).await;
        assert!(matches!(
            result,
            Err(XmppError::Stanza {
                condition: crate::StanzaErrorCondition::BadRequest,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn test_route_iq_local_muc_jingle_routes_when_sender_and_target_are_room_occupants() {
        let (router, registry) = create_router_with_test_room().await;
        let (tx, mut rx) = mpsc::channel(16);
        let target_room_jid: FullJid = "room@muc.waddle.social/target-nick".parse().unwrap();
        registry.register(target_room_jid, tx);

        let sender_jid: FullJid = "sender@waddle.social/resource".parse().unwrap();
        let iq = parse_iq(
            r#"<iq xmlns='jabber:client' type='set' from='sender@waddle.social/resource' to='room@muc.waddle.social/target-nick' id='iq-jingle-2'>
                <jingle xmlns='urn:xmpp:jingle:1' action='session-info' sid='abc123'/>
            </iq>"#,
        );

        let result = router.route_iq(iq, &sender_jid).await.unwrap();
        assert!(matches!(
            result,
            RoutingResult::DeliveredLocal {
                delivered_count: 1,
                offline_count: 0
            }
        ));
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn test_route_iq_local_muc_jingle_rejects_non_occupant_sender() {
        let (router, _registry) = create_router_with_test_room().await;
        let sender_jid: FullJid = "outsider@waddle.social/resource".parse().unwrap();
        let iq = parse_iq(
            r#"<iq xmlns='jabber:client' type='set' from='outsider@waddle.social/resource' to='room@muc.waddle.social/target-nick' id='iq-jingle-3'>
                <jingle xmlns='urn:xmpp:jingle:1' action='session-info' sid='abc123'/>
            </iq>"#,
        );

        let result = router.route_iq(iq, &sender_jid).await;
        assert!(matches!(
            result,
            Err(XmppError::Stanza {
                condition: crate::StanzaErrorCondition::Forbidden,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn test_route_iq_local_muc_jingle_rejects_missing_target_occupant() {
        let (router, _registry) = create_router_with_test_room().await;
        let sender_jid: FullJid = "sender@waddle.social/resource".parse().unwrap();
        let iq = parse_iq(
            r#"<iq xmlns='jabber:client' type='set' from='sender@waddle.social/resource' to='room@muc.waddle.social/missing-nick' id='iq-jingle-4'>
                <jingle xmlns='urn:xmpp:jingle:1' action='session-info' sid='abc123'/>
            </iq>"#,
        );

        let result = router.route_iq(iq, &sender_jid).await;
        assert!(matches!(
            result,
            Err(XmppError::Stanza {
                condition: crate::StanzaErrorCondition::ItemNotFound,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn test_route_iq_local_validates_inbound_jingle_sender_membership() {
        let (router, _registry) = create_router_with_test_room().await;
        let iq = parse_iq(
            r#"<iq xmlns='jabber:client' type='set' from='outsider@waddle.social/resource' to='room@muc.waddle.social/target-nick' id='iq-jingle-5'>
                <jingle xmlns='urn:xmpp:jingle:1' action='session-info' sid='abc123'/>
            </iq>"#,
        );

        let result = router.route_iq_local(iq).await;
        assert!(matches!(
            result,
            Err(XmppError::Stanza {
                condition: crate::StanzaErrorCondition::Forbidden,
                ..
            })
        ));
    }

    #[test]
    fn routes_sfu_domain_to_local_sfu() {
        let config = RouterConfig::new("waddle.social".to_string());
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);
        let dest = router.get_destination_for_domain("sfu.waddle.social");
        assert_eq!(dest, RoutingDestination::LocalSfu);
    }

    #[test]
    fn sfu_domain_does_not_match_muc_or_local() {
        let config = RouterConfig::new("waddle.social".to_string());
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry, None);
        assert_eq!(router.get_destination_for_domain("waddle.social"), RoutingDestination::Local);
        assert_eq!(router.get_destination_for_domain("muc.waddle.social"), RoutingDestination::LocalMuc);
        assert_eq!(router.get_destination_for_domain("sfu.waddle.social"), RoutingDestination::LocalSfu);
    }
}
