//! Stanza routing for local WebSocket XMPP delivery.
//!
//! This module provides the `StanzaRouter` which determines whether a JID is local
//! or remote and routes local stanzas through the in-process connection registry.
//!
//! Message stanzas no longer go through this router: the per-connection
//! sans-I/O dispatcher chain
//! ([`crate::protocol::handlers::register_default_message_handlers`]) and
//! the `RouteToConnection` interpreter arm own message routing as of #229
//! PR16. Only IQ and presence still flow through `StanzaRouter`.
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
//!     user_registry,
//! );
//!
//! // Route IQ stanzas locally.
//! router.route_iq(iq, &sender_jid).await?;
//! ```

use std::sync::Arc;

use jid::{FullJid, Jid};
use kameo::actor::ActorRef;
use tracing::{debug, info, instrument, Span};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::presence::Presence;

use crate::registry::{ConnectionRegistry, SendResult, UserRegistryActor};
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
    /// Actor-authoritative per-user registry (ADR-0017 Phase 3 Slice 9):
    /// bare-JID resource enumeration sources from here, not the DashMap
    /// `connection_registry` above.
    user_registry: ActorRef<UserRegistryActor>,
}

impl StanzaRouter {
    /// Create a new stanza router.
    ///
    /// # Arguments
    ///
    /// * `config` - Router configuration including local domain
    /// * `connection_registry` - Registry of local connections for message delivery
    /// * `user_registry` - Actor-authoritative per-user registry for bare-JID
    ///   resource selection (ADR-0017 Phase 3 Slice 9)
    pub fn new(
        config: RouterConfig,
        connection_registry: Arc<ConnectionRegistry>,
        user_registry: ActorRef<UserRegistryActor>,
    ) -> Self {
        info!(
            local_domain = %config.local_domain,
            muc_domain = %config.muc_domain,
            "StanzaRouter initialized"
        );

        Self {
            config,
            connection_registry,
            user_registry,
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

    /// Route a presence stanza to its destination.
    #[instrument(
        skip(self, presence, _sender_jid),
        fields(
            to = tracing::field::Empty,
            from = %_sender_jid,
            presence_type = ?presence.type_
        )
    )]
    pub async fn route_presence(
        &self,
        presence: Presence,
        _sender_jid: &FullJid,
    ) -> Result<RoutingResult, XmppError> {
        let to_jid = match &presence.to {
            Some(jid) => {
                // Recorded on arrival rather than as a span-creation
                // field so the JID renders through `Display` instead of
                // `Debug`'s `Bare(BareJid("…"))` (#1439), and so an
                // absent destination leaves the attribute off entirely.
                Span::current().record("to", tracing::field::display(jid));
                jid
            }
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
                // Send to all resources (ADR-0017 Phase 3 Slice 9: sourced
                // from the actor-authoritative registry, not the DashMap).
                let resources =
                    crate::registry::get_resources_for_user(&self.user_registry, &bare_jid).await;
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
    #[instrument(
        skip(self, iq, sender_jid),
        fields(to = tracing::field::Empty, from = %sender_jid)
    )]
    pub async fn route_iq(&self, iq: Iq, sender_jid: &FullJid) -> Result<RoutingResult, XmppError> {
        let to_jid = match iq.to() {
            Some(jid) => {
                // `Display`, not `Debug` — see `route_presence` (#1439).
                Span::current().record("to", tracing::field::display(jid));
                jid.clone()
            }
            None => {
                // IQ without 'to' is directed at the server
                debug!("IQ has no destination JID (server query)");
                return Ok(RoutingResult::NoDestination);
            }
        };

        match self.get_destination(&to_jid) {
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
            .to()
            .cloned()
            .ok_or_else(|| XmppError::bad_request(Some("IQ has no destination".to_string())))?;

        self.route_iq_local_unchecked(iq, to_jid).await
    }

    async fn route_iq_local_unchecked(
        &self,
        iq: Iq,
        to_jid: Jid,
    ) -> Result<RoutingResult, XmppError> {
        let stanza = Stanza::Iq(Box::new(iq));

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
                // (ADR-0017 Phase 3 Slice 9: actor-authoritative registry).
                let resources =
                    crate::registry::get_resources_for_user(&self.user_registry, &bare_jid).await;

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
            .to()
            .cloned()
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
mod tests;
