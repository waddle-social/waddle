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
    /// Whether S2S federation is enabled
    pub federation_enabled: bool,
}

impl RouterConfig {
    /// Create a new router configuration.
    pub fn new(local_domain: String) -> Self {
        let muc_domain = format!("muc.{}", local_domain);
        Self {
            local_domain,
            muc_domain,
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
            s2s_pool,
        }
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
            RoutingDestination::Local | RoutingDestination::LocalMuc
        )
    }

    /// Check if a JID is for the local MUC service.
    pub fn is_muc_jid(&self, jid: &Jid) -> bool {
        matches!(self.get_destination(jid), RoutingDestination::LocalMuc)
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
            RoutingDestination::Local => {
                self.route_message_local(message).await
            }
            RoutingDestination::LocalMuc => {
                // MUC messages should be handled by the MUC room registry,
                // not by this router directly. Return as local.
                debug!("Message to MUC should be handled by room registry");
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
            match self.connection_registry.send_to(resource_jid, stanza.clone()).await {
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

        let pool = self.s2s_pool.as_ref().ok_or_else(|| {
            XmppError::internal("S2S pool not available".to_string())
        })?;

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
            RoutingDestination::Local => {
                self.route_presence_local(presence).await
            }
            RoutingDestination::LocalMuc => {
                // MUC presence should be handled by the MUC room registry
                debug!("Presence to MUC should be handled by room registry");
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
    pub async fn route_presence_local(&self, presence: Presence) -> Result<RoutingResult, XmppError> {
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
                    SendResult::Sent => {
                        Ok(RoutingResult::DeliveredLocal {
                            delivered_count: 1,
                            offline_count: 0,
                        })
                    }
                    _ => {
                        Ok(RoutingResult::DeliveredLocal {
                            delivered_count: 0,
                            offline_count: 1,
                        })
                    }
                }
            }
            Err(bare_jid) => {
                // Send to all resources
                let resources = self.connection_registry.get_resources_for_user(&bare_jid);
                let mut delivered = 0;
                let mut offline = 0;

                for resource_jid in resources {
                    match self.connection_registry.send_to(&resource_jid, stanza.clone()).await {
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

        let pool = self.s2s_pool.as_ref().ok_or_else(|| {
            XmppError::internal("S2S pool not available".to_string())
        })?;

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
    pub async fn route_iq(
        &self,
        iq: Iq,
        _sender_jid: &FullJid,
    ) -> Result<RoutingResult, XmppError> {
        let to_jid = match &iq.to {
            Some(jid) => jid,
            None => {
                // IQ without 'to' is directed at the server
                debug!("IQ has no destination JID (server query)");
                return Ok(RoutingResult::NoDestination);
            }
        };

        match self.get_destination(to_jid) {
            RoutingDestination::Local => {
                self.route_iq_local(iq).await
            }
            RoutingDestination::LocalMuc => {
                // MUC IQs should be handled by the MUC room registry
                debug!("IQ to MUC should be handled by room registry");
                Ok(RoutingResult::DeliveredLocal {
                    delivered_count: 0,
                    offline_count: 0,
                })
            }
            RoutingDestination::Remote { domain } => {
                self.route_iq_remote(iq, &domain).await
            }
        }
    }

    /// Route IQ to local users.
    ///
    /// This is also used by the S2S listener to route inbound IQs
    /// from remote servers to local recipients.
    pub async fn route_iq_local(&self, iq: Iq) -> Result<RoutingResult, XmppError> {
        // Clone the destination JID before moving iq into the stanza
        let to_jid = iq.to.clone().ok_or_else(|| {
            XmppError::bad_request(Some("IQ has no destination".to_string()))
        })?;

        let stanza = Stanza::Iq(iq);

        match to_jid.try_into_full() {
            Ok(full_jid) => {
                match self.connection_registry.send_to(&full_jid, stanza).await {
                    SendResult::Sent => {
                        Ok(RoutingResult::DeliveredLocal {
                            delivered_count: 1,
                            offline_count: 0,
                        })
                    }
                    _ => {
                        Ok(RoutingResult::DeliveredLocal {
                            delivered_count: 0,
                            offline_count: 1,
                        })
                    }
                }
            }
            Err(bare_jid) => {
                // For bare JID, send to first available resource
                let resources = self.connection_registry.get_resources_for_user(&bare_jid);

                if let Some(resource_jid) = resources.first() {
                    match self.connection_registry.send_to(resource_jid, stanza).await {
                        SendResult::Sent => {
                            Ok(RoutingResult::DeliveredLocal {
                                delivered_count: 1,
                                offline_count: 0,
                            })
                        }
                        _ => {
                            Ok(RoutingResult::DeliveredLocal {
                                delivered_count: 0,
                                offline_count: 1,
                            })
                        }
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

    /// Route IQ to a remote server via S2S.
    async fn route_iq_remote(
        &self,
        iq: Iq,
        domain: &str,
    ) -> Result<RoutingResult, XmppError> {
        if !self.is_federation_enabled() {
            return Ok(RoutingResult::FederationDisabled);
        }

        let pool = self.s2s_pool.as_ref().ok_or_else(|| {
            XmppError::internal("S2S pool not available".to_string())
        })?;

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

    fn create_test_config() -> RouterConfig {
        RouterConfig::new("waddle.social".to_string())
    }

    fn create_test_jid(jid_str: &str) -> Jid {
        jid_str.parse().unwrap()
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
        let mut message = Message::new(Some(Jid::from("user@waddle.social".parse::<BareJid>().unwrap())));
        message.id = Some("test-123".to_string());

        let result = router.route_message(message, &sender_jid).await.unwrap();

        match result {
            RoutingResult::DeliveredLocal { delivered_count, offline_count } => {
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
        let message = Message::new(Some(Jid::from("user@example.com".parse::<BareJid>().unwrap())));

        let result = router.route_message(message, &sender_jid).await.unwrap();

        assert!(matches!(result, RoutingResult::FederationDisabled));
    }
}
