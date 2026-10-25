//! Keyed authentication of extension launch context and descriptor content.

use super::{
    constant_time_eq, launch_expiry_at, launch_payload_digest, pubsub_node_placeholder_value,
    sign_launch_token, submitted_launch_payload_digest, DateTime, ExtensionEffect,
    ExtensionEnvelope, ExtensionManager, LaunchTokenVerification, PluginId, RoomJid, Utc,
};
use crate::types::LaunchDescriptor;
use crate::types::LaunchToken;
use crate::types::PubSubNode;

impl ExtensionManager {
    pub(super) fn sign_effects(&self, mut effects: Vec<ExtensionEffect>) -> Vec<ExtensionEffect> {
        for effect in &mut effects {
            match effect {
                ExtensionEffect::EnrichMessage(envelope) => {
                    self.sign_envelope(envelope);
                }
                ExtensionEffect::PublishRoomResult(_)
                | ExtensionEffect::PublishPubSub(_)
                | ExtensionEffect::ReferenceArtifact(_)
                | ExtensionEffect::CommandForm(_)
                | ExtensionEffect::HostWarning(_)
                | ExtensionEffect::Noop => {}
            }
        }
        effects
    }

    /// Sign every launch in an enrichment envelope using the current time; descriptors
    /// are unchanged when no signing key is installed.
    pub fn sign_envelope(&self, envelope: &mut ExtensionEnvelope) {
        self.sign_envelope_at(envelope, Utc::now());
    }

    /// Sign one envelope using a single captured time for generated expiries.
    pub fn sign_envelope_at(&self, envelope: &mut ExtensionEnvelope, now: DateTime<Utc>) {
        for enrichment in &mut envelope.enrichments {
            for launch in &mut enrichment.launches {
                self.sign_launch(launch, now);
            }
        }
    }

    /// Whether the named loaded plugin may emit this enrichment envelope under its
    /// manifest and grants.
    #[must_use]
    pub fn validate_envelope_for_plugin(
        &self,
        plugin: &PluginId,
        envelope: &ExtensionEnvelope,
    ) -> bool {
        self.actors
            .iter()
            .find(|actor| actor.manifest().id == *plugin)
            .is_some_and(|actor| {
                actor.validate_effect(&ExtensionEffect::EnrichMessage(envelope.clone()))
            })
    }

    /// Extract the first valid bare room JID from a matching declared node's `{room}`
    /// placeholder across loaded plugins.
    #[must_use]
    pub fn room_for_pubsub_node(&self, node: &PubSubNode) -> Option<RoomJid> {
        for actor in &self.actors {
            let manifest = actor.manifest();
            if let Some(room) = manifest.pubsub_nodes.iter().find_map(|pattern| {
                pubsub_node_placeholder_value(pattern.as_str(), node.as_str(), "room")
                    .and_then(|room| RoomJid::try_new(room).ok())
            }) {
                return Some(room);
            }
        }
        None
    }

    /// Extract a valid bare room JID from the named plugin's matching PubSub node pattern.
    #[must_use]
    pub fn room_for_plugin_pubsub_node(
        &self,
        plugin: &PluginId,
        node: &PubSubNode,
    ) -> Option<RoomJid> {
        self.actors
            .iter()
            .find(|actor| actor.manifest().id == *plugin)
            .and_then(|actor| {
                actor.manifest().pubsub_nodes.iter().find_map(|pattern| {
                    pubsub_node_placeholder_value(pattern.as_str(), node.as_str(), "room")
                        .and_then(|room| RoomJid::try_new(room).ok())
                })
            })
    }

    fn sign_launch(&self, launch: &mut LaunchDescriptor, now: DateTime<Utc>) {
        let Some(key) = self.launch_signing_key.as_deref() else {
            return;
        };
        if launch.expires_at.is_none() {
            launch.expires_at = launch_expiry_at(now);
        }
        let Some(expires_at) = launch.expires_at.as_ref() else {
            return;
        };
        let payload_digest = launch_payload_digest(&launch.payloads);
        let token = sign_launch_token(
            key,
            &launch.plugin,
            &launch.action,
            &launch.id,
            &launch.context,
            Some(expires_at),
            &payload_digest,
        );
        launch.token = LaunchToken::try_new(token).ok();
    }

    pub(super) fn verify_launch_token(&self, request: LaunchTokenVerification<'_>) -> bool {
        let Some(key) = self.launch_signing_key.as_deref() else {
            return false;
        };
        if let Some(expires_at) = request.expires_at {
            let Ok(expires_at) = DateTime::parse_from_rfc3339(expires_at.as_str()) else {
                return false;
            };
            if expires_at.with_timezone(&Utc) <= Utc::now() {
                return false;
            }
        }
        let payload_digest = submitted_launch_payload_digest(request.fields);
        let expected = sign_launch_token(
            key,
            request.plugin,
            request.action,
            request.launch_id,
            request.context,
            request.expires_at,
            &payload_digest,
        );
        constant_time_eq(expected.as_bytes(), request.token.as_bytes())
    }
}
