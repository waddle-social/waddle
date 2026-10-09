//! Host-owned authority for a single started observation. The WASM resource
//! never exposes its key representation and each use rechecks durable authority.
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use waddle_extensions::host_tools::{
    ExtensionDeliveryCapability, HostToolError, HostToolErrorCode, InvocationContext,
    InvocationKind,
};
use waddle_extensions::{DisplayText, RoomMessageSource};
use waddle_xmpp::ingress::DeliveryKey;

use crate::ingress_uow::{ObservationWork, RoomObservationRepository};
use crate::server::routes::websocket::WebSocketState;

pub(super) struct ObservationDeliveryCapability {
    state: Weak<WebSocketState>,
    work: ObservationWork,
    key: DeliveryKey,
}

impl ObservationDeliveryCapability {
    pub(super) fn new(state: &Arc<WebSocketState>, work: &ObservationWork) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::downgrade(state),
            work: work.clone(),
            key: work.delivery_key(),
        })
    }
}

fn unavailable() -> HostToolError {
    HostToolError {
        code: HostToolErrorCode::TemporaryFailure,
        message: DisplayText::new("observation delivery authority is unavailable")
            .expect("static error"),
    }
}

fn denied() -> HostToolError {
    HostToolError::denied(
        DisplayText::new("observation delivery authority is no longer current")
            .expect("static denial"),
    )
}

#[async_trait]
impl ExtensionDeliveryCapability for ObservationDeliveryCapability {
    async fn validate(
        &self,
        context: &InvocationContext,
        source: &RoomMessageSource,
    ) -> Result<(), HostToolError> {
        if context.kind != InvocationKind::RoomMessageObserve
            || context.plugin_id != self.work.subscription.plugin
            || context.source_room.as_ref() != Some(&self.work.subscription.room)
            || source != &self.work.source
            || self.key != self.work.delivery_key()
        {
            return Err(denied());
        }
        let state = self.state.upgrade().ok_or_else(denied)?;
        if state
            .deps
            .protocol
            .ingress
            .observation_cancellation()
            .is_cancelled()
        {
            return Err(denied());
        }
        let mut tx = state
            .deps
            .protocol
            .ingress
            .observation_transaction()
            .await
            .map_err(|_| unavailable())?;
        let valid =
            RoomObservationRepository::validate_started(&mut tx, &self.work, crate::time::now_ms())
                .await
                .map_err(|_| unavailable())?;
        tx.commit().await.map_err(|_| unavailable())?;
        if valid {
            Ok(())
        } else {
            Err(denied())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sha2::{Digest, Sha256};
    use waddle_extensions::{
        ConfiguredRoomObserver, ObservationGeneration, PluginId, RoomObservationScope,
        Sha256Digest, WaddleId,
    };
    use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey, SemanticDigest};
    use waddle_xmpp_core::xep0359::{add_origin_id, add_stanza_id, StanzaId};
    use xmpp_parsers::message::{Lang, Message, MessageType};

    use crate::ingress::test_support::IngressFixture;
    use crate::ingress_uow::{
        initialize_room_observations, CanonicalMessageRepository, CapturedRoomSource,
        EffectIntentRepository,
    };

    async fn fixture() -> (
        IngressFixture,
        Arc<WebSocketState>,
        ObservationWork,
        InvocationContext,
    ) {
        let fixture = IngressFixture::sqlite().await;
        initialize_room_observations(&fixture.db)
            .await
            .expect("schema");
        let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_durable_ingress(&fixture).await;
        let room = "room@conference.example.org".parse().expect("room");
        let sender: jid::BareJid = "author@example.org".parse().expect("sender");
        let observer = ConfiguredRoomObserver {
            plugin: PluginId::new("capability-fixture").expect("plugin"),
            generation: ObservationGeneration::new(1).expect("generation"),
            identity: Sha256Digest::new("a".repeat(64)).expect("identity"),
            scope: RoomObservationScope::Rooms(vec![room]),
            max_concurrent: 1,
        };
        let subscription = super::super::subscription(
            &observer,
            "room@conference.example.org".parse().expect("room"),
        );
        let mut message = Message::new(Some(subscription.room.clone().into()));
        message.type_ = MessageType::Groupchat;
        message.from = Some(
            "room@conference.example.org/author"
                .parse()
                .expect("occupant"),
        );
        message.bodies.insert(Lang::new(), "body".into());
        add_origin_id(&mut message, "origin");
        add_stanza_id(
            &mut message,
            &StanzaId::new("stanza", subscription.room.clone().into()),
        );
        let intent = IngressEffectIntent::RoomObserver {
            room: subscription.room.clone(),
            requester: sender.clone(),
            sender: "room@conference.example.org/author"
                .parse()
                .expect("occupant"),
            plugin: subscription.plugin.clone(),
            generation: subscription.generation,
            identity: subscription.identity.clone(),
            correction_target: None,
        };
        let key = MessageKey::new();
        let now = Utc::now();
        let mut tx = fixture.uow.begin().await.expect("seed");
        CanonicalMessageRepository::record_message(
            &mut tx,
            key,
            &SemanticDigest::from_storage(1, Sha256::digest(b"body").into()).expect("digest"),
            None,
        )
        .await
        .expect("canonical");
        RoomObservationRepository::sync_configured(&mut tx, &[observer], now.timestamp_millis())
            .await
            .expect("config");
        EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
            .await
            .expect("intent");
        RoomObservationRepository::capture(
            &mut tx,
            CapturedRoomSource {
                key,
                room: &subscription.room,
                message: &message,
                sender: &sender,
                intents: &[intent],
                observed_at: now,
                correction_target: None,
            },
        )
        .await
        .expect("capture");
        tx.commit().await.expect("seed commit");
        let mut tx = fixture.uow.begin().await.expect("start");
        let work = RoomObservationRepository::claim(&mut tx, &subscription, now.timestamp_millis())
            .await
            .expect("claim")
            .expect("work");
        assert!(
            RoomObservationRepository::start(&mut tx, &work, now.timestamp_millis())
                .await
                .expect("start")
        );
        tx.commit().await.expect("start commit");
        let context = InvocationContext {
            waddle_id: WaddleId::new("room-observation").expect("context"),
            plugin_id: subscription.plugin,
            requester: None,
            source_room: Some(subscription.room),
            kind: InvocationKind::RoomMessageObserve,
            provider_room_grants: Vec::new(),
        };
        (fixture, state, work, context)
    }

    #[tokio::test]
    async fn capability_rechecks_context_source_lease_and_generation() {
        let (fixture, state, work, context) = fixture().await;
        let capability = ObservationDeliveryCapability::new(&state, &work);
        assert!(capability.validate(&context, &work.source).await.is_ok());
        let mut wrong = context.clone();
        wrong.plugin_id = PluginId::new("different-plugin").expect("plugin");
        assert!(capability.validate(&wrong, &work.source).await.is_err());
        let mut source = work.source.clone();
        source.revision = waddle_extensions::MessageRevision::new(1);
        assert!(capability.validate(&context, &source).await.is_err());
        fixture
            .execute(
                "UPDATE extension_room_observation_work SET lease_id = ? WHERE id = ?",
                crate::db_params![uuid::Uuid::now_v7().to_string(), work.id.to_string()],
            )
            .await;
        assert!(capability.validate(&context, &work.source).await.is_err());
        fixture
            .execute(
                "UPDATE extension_room_observation_work SET lease_id = ? WHERE id = ?",
                crate::db_params![work.lease.to_string(), work.id.to_string()],
            )
            .await;
        assert!(capability.validate(&context, &work.source).await.is_ok());
        fixture
            .execute("UPDATE extension_room_observers SET generation = 2", ())
            .await;
        assert!(capability.validate(&context, &work.source).await.is_err());
        assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
        fixture.close().await;
    }

    #[tokio::test]
    async fn capability_rejects_expiry_and_retraction_before_provider_reentry() {
        let (fixture, state, work, context) = fixture().await;
        let capability = ObservationDeliveryCapability::new(&state, &work);
        fixture
            .execute(
                "UPDATE extension_room_observation_work SET lease_until_ms = 0 WHERE id = ?",
                crate::db_params![work.id.to_string()],
            )
            .await;
        assert!(capability.validate(&context, &work.source).await.is_err());
        fixture
            .execute(
                "UPDATE extension_room_observation_work SET lease_until_ms = ? WHERE id = ?",
                crate::db_params![crate::time::now_ms() + 180_000, work.id.to_string()],
            )
            .await;
        assert!(capability.validate(&context, &work.source).await.is_ok());
        fixture
            .execute("UPDATE extension_room_sources SET retracted = 1", ())
            .await;
        assert!(capability.validate(&context, &work.source).await.is_err());
        assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
        fixture.close().await;
    }
}
