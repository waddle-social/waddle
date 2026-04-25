//! Stanza routing for local WebSocket XMPP delivery.
//!
//! This module provides the `StanzaRouter` which determines whether a JID is local
//! or remote and routes local stanzas through the in-process connection registry.
//!
//! # Routing Logic
//!
//! For each stanza, the router:
//! 1. Extracts the destination JID from the stanza
//! 2. Checks if the JID's domain matches the local domain
//! 3. If local: routes via the `ConnectionRegistry` to local users
//! 4. If remote: returns `RemoteUnsupported`
//!
//! # Example
//!
//! ```ignore
//! use waddle_xmpp::routing::{RouterConfig, StanzaRouter};
//!
//! let router = StanzaRouter::new(
//!     RouterConfig::new("waddle.social".to_string()),
//!     connection_registry,
//! );
//!
//! // Route a message locally or return RemoteUnsupported for remote domains.
//! router.route_message(message, sender_jid).await?;
//! ```

use std::sync::Arc;

use jid::{BareJid, FullJid, Jid};
use tracing::{debug, info, instrument};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;
use xmpp_parsers::presence::Presence;

use crate::registry::{ConnectionRegistry, SendResult};
use crate::Stanza;
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
    /// No destination JID in stanza
    NoDestination,
    /// Remote domains are not routed by the WebSocket-only server.
    RemoteUnsupported,
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
}

impl RouterConfig {
    /// Create a new router configuration.
    pub fn new(local_domain: String) -> Self {
        let muc_domain = format!("muc.{}", local_domain);
        let spaces_domain = format!("spaces.{}", local_domain);
        Self {
            local_domain,
            muc_domain,
            spaces_domain,
        }
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
    /// JID is on a remote server.
    Remote {
        /// The remote domain
        domain: String,
    },
}

/// Stanza router for local message delivery.
///
/// The router examines each stanza's destination JID and determines whether
/// to deliver locally via the connection registry.
pub struct StanzaRouter {
    /// Router configuration
    config: RouterConfig,
    /// Connection registry for local users
    connection_registry: Arc<ConnectionRegistry>,
}

impl StanzaRouter {
    /// Create a new stanza router.
    ///
    /// # Arguments
    ///
    /// * `config` - Router configuration including local domain
    /// * `connection_registry` - Registry of local connections for message delivery
    pub fn new(config: RouterConfig, connection_registry: Arc<ConnectionRegistry>) -> Self {
        info!(
            local_domain = %config.local_domain,
            muc_domain = %config.muc_domain,
            "StanzaRouter initialized"
        );

        Self {
            config,
            connection_registry,
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

    /// Remote routing is disabled for the WebSocket-only server.
    pub fn is_remote_routing_enabled(&self) -> bool {
        false
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
        )
    }

    /// Check if a JID is for the local MUC service.
    pub fn is_muc_jid(&self, jid: &Jid) -> bool {
        matches!(self.get_destination(jid), RoutingDestination::LocalMuc)
    }

    /// Check if a JID is outside the local WebSocket-served domains.
    pub fn is_remote_jid(&self, jid: &Jid) -> bool {
        matches!(self.get_destination(jid), RoutingDestination::Remote { .. })
    }

    /// Route a message stanza to its destination.
    ///
    /// For local recipients, the message is sent via the connection registry.
    /// Remote recipients are not routed by the WebSocket-only server.
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
            RoutingDestination::LocalMuc | RoutingDestination::LocalSpaces => {
                // MUC/Spaces messages should be handled by their respective services,
                // not by this router directly. Return as local.
                debug!("Message to local service should be handled by service handler");
                Ok(RoutingResult::DeliveredLocal {
                    delivered_count: 0,
                    offline_count: 0,
                })
            }
            RoutingDestination::Remote { domain } => self.route_message_remote(&domain).await,
        }
    }

    /// Route a message to local users.
    ///
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
            }
        }

        Ok(RoutingResult::DeliveredLocal {
            delivered_count,
            offline_count,
        })
    }

    /// Reject a message addressed to a remote server.
    async fn route_message_remote(&self, domain: &str) -> Result<RoutingResult, XmppError> {
        debug!(domain = %domain, "Remote routing is not supported");
        Ok(RoutingResult::RemoteUnsupported)
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
            RoutingDestination::LocalMuc | RoutingDestination::LocalSpaces => {
                // MUC/Spaces presence should be handled by their respective services
                debug!("Presence to local service should be handled by service handler");
                Ok(RoutingResult::DeliveredLocal {
                    delivered_count: 0,
                    offline_count: 0,
                })
            }
            RoutingDestination::Remote { domain } => self.route_presence_remote(&domain).await,
        }
    }

    /// Route presence to local users.
    ///
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

    /// Reject presence addressed to a remote server.
    async fn route_presence_remote(&self, domain: &str) -> Result<RoutingResult, XmppError> {
        debug!(domain = %domain, "Remote routing is not supported");
        Ok(RoutingResult::RemoteUnsupported)
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
                // Spaces IQs should be handled by their service
                debug!("IQ to local service should be handled by service handler");
                Ok(RoutingResult::DeliveredLocal {
                    delivered_count: 0,
                    offline_count: 0,
                })
            }
            RoutingDestination::Remote { domain } => self.route_iq_remote(&domain).await,
        }
    }

    /// Route IQ to local users.
    ///
    pub async fn route_iq_local(&self, iq: Iq) -> Result<RoutingResult, XmppError> {
        // Clone the destination JID before moving iq into the stanza
        let to_jid = iq
            .to
            .clone()
            .ok_or_else(|| XmppError::bad_request(Some("IQ has no destination".to_string())))?;

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
    async fn route_iq_local_muc(
        &self,
        iq: Iq,
        _sender_jid: Option<&FullJid>,
    ) -> Result<RoutingResult, XmppError> {
        let to_jid = iq
            .to
            .clone()
            .ok_or_else(|| XmppError::bad_request(Some("IQ has no destination".to_string())))?;
        self.route_iq_local_unchecked(iq, to_jid).await
    }

    /// Reject IQ addressed to a remote server.
    async fn route_iq_remote(&self, domain: &str) -> Result<RoutingResult, XmppError> {
        debug!(domain = %domain, "Remote routing is not supported");
        Ok(RoutingResult::RemoteUnsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn test_router_config() {
        let config = RouterConfig::new("example.com".to_string());
        assert_eq!(config.local_domain, "example.com");
        assert_eq!(config.muc_domain, "muc.example.com");

        let config = config.with_muc_domain("chat.example.com".to_string());
        assert_eq!(config.muc_domain, "chat.example.com");
    }

    #[test]
    fn test_get_destination_local() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry);

        let jid = create_test_jid("user@waddle.social");
        assert_eq!(router.get_destination(&jid), RoutingDestination::Local);

        let jid = create_test_jid("user@waddle.social/resource");
        assert_eq!(router.get_destination(&jid), RoutingDestination::Local);
    }

    #[test]
    fn test_get_destination_muc() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry);

        let jid = create_test_jid("room@muc.waddle.social");
        assert_eq!(router.get_destination(&jid), RoutingDestination::LocalMuc);

        let jid = create_test_jid("room@muc.waddle.social/nick");
        assert_eq!(router.get_destination(&jid), RoutingDestination::LocalMuc);
    }

    #[test]
    fn test_get_destination_spaces() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry);

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
        let router = StanzaRouter::new(config, registry);

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
        let router = StanzaRouter::new(config, registry);

        assert!(router.is_local_jid(&create_test_jid("user@waddle.social")));
        assert!(router.is_local_jid(&create_test_jid("room@muc.waddle.social")));
        assert!(router.is_local_jid(&create_test_jid("spaces.waddle.social")));
        assert!(!router.is_local_jid(&create_test_jid("user@example.com")));
    }

    #[test]
    fn test_is_muc_jid() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry);

        assert!(!router.is_muc_jid(&create_test_jid("user@waddle.social")));
        assert!(router.is_muc_jid(&create_test_jid("room@muc.waddle.social")));
        assert!(!router.is_muc_jid(&create_test_jid("user@example.com")));
    }

    #[test]
    fn test_is_remote_jid() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry);

        assert!(!router.is_remote_jid(&create_test_jid("user@waddle.social")));
        assert!(!router.is_remote_jid(&create_test_jid("room@muc.waddle.social")));
        assert!(router.is_remote_jid(&create_test_jid("user@example.com")));
    }

    #[test]
    fn test_federation_disabled_by_default() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry);

        assert!(!router.is_remote_routing_enabled());
    }

    #[tokio::test]
    async fn test_route_message_local_not_connected() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry);

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
        let router = StanzaRouter::new(config, registry);

        let sender_jid: FullJid = "sender@waddle.social/resource".parse().unwrap();
        let message = Message::new(None);

        let result = router.route_message(message, &sender_jid).await.unwrap();

        assert!(matches!(result, RoutingResult::NoDestination));
    }

    #[tokio::test]
    async fn test_route_message_remote_unsupported() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let router = StanzaRouter::new(config, registry);

        let sender_jid: FullJid = "sender@waddle.social/resource".parse().unwrap();
        let message = Message::new(Some(Jid::from(
            "user@example.com".parse::<BareJid>().unwrap(),
        )));

        let result = router.route_message(message, &sender_jid).await.unwrap();

        assert!(matches!(result, RoutingResult::RemoteUnsupported));
    }

    #[tokio::test]
    async fn test_route_iq_local_muc_bare_non_jingle_routes_to_connected_resource() {
        let config = create_test_config();
        let registry = Arc::new(ConnectionRegistry::new());
        let (tx, mut rx) = mpsc::channel(16);
        let full_room_jid: FullJid = "room@muc.waddle.social/nick".parse().unwrap();
        registry.register(full_room_jid, tx);
        let router = StanzaRouter::new(config, registry);

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
}
