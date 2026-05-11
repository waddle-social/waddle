use super::*;

impl ExtensionManager {
    pub(super) fn sign_effects(&self, mut effects: Vec<ExtensionEffect>) -> Vec<ExtensionEffect> {
        for effect in &mut effects {
            match effect {
                ExtensionEffect::EnrichMessage(envelope) => {
                    self.sign_envelope(envelope);
                }
                ExtensionEffect::PublishPubSub(_)
                | ExtensionEffect::ReferenceArtifact(_)
                | ExtensionEffect::CommandForm(_)
                | ExtensionEffect::HostWarning(_)
                | ExtensionEffect::Noop => {}
            }
        }
        effects
    }

    pub fn sign_envelope(&self, envelope: &mut ExtensionEnvelope) {
        for enrichment in &mut envelope.enrichments {
            for launch in &mut enrichment.launches {
                self.sign_launch(launch);
            }
        }
    }

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

    pub fn room_for_pubsub_node(&self, node: &crate::types::PubSubNode) -> Option<RoomJid> {
        for actor in &self.actors {
            let manifest = actor.manifest();
            if let Some(room) = manifest.pubsub_nodes.iter().find_map(|pattern| {
                pubsub_node_placeholder_value(pattern.as_str(), node.as_str(), "room")
                    .and_then(|room| RoomJid::new(room).ok())
            }) {
                return Some(room);
            }
        }
        None
    }

    pub fn room_for_plugin_pubsub_node(
        &self,
        plugin: &PluginId,
        node: &crate::types::PubSubNode,
    ) -> Option<RoomJid> {
        self.actors
            .iter()
            .find(|actor| actor.manifest().id == *plugin)
            .and_then(|actor| {
                actor.manifest().pubsub_nodes.iter().find_map(|pattern| {
                    pubsub_node_placeholder_value(pattern.as_str(), node.as_str(), "room")
                        .and_then(|room| RoomJid::new(room).ok())
                })
            })
    }

    fn sign_launch(&self, launch: &mut crate::types::LaunchDescriptor) {
        let Some(key) = self.launch_signing_key.as_deref() else {
            return;
        };
        let expires_at = launch
            .expires_at
            .get_or_insert_with(|| default_launch_expiry().expect("generated expiry is valid"));
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
        launch.token = crate::types::LaunchToken::new(token).ok();
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
