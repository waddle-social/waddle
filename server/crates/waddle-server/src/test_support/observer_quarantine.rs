//! Disposable PostgreSQL-only observer start experiment.
//!
//! This module is compiled only with `test-support`. It is not selected by
//! startup or by the room-observer actor. Its caller must install the fixture
//! guard in a disposable database and must fence every non-fixture writer.
//! The starter and claimer are trusted test processes; this does not attest
//! production roles, protect against a compromised fixture role or privileged
//! DDL, or establish observer/canonical garbage-collection safety. The grant
//! only controls the fixture callback, not a real guest or provider.

use anyhow::{ensure, Context};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use waddle_extensions::{
    ConfiguredRoomObserver, DisplayText, MessageRevision, ObservationGeneration, PluginId,
    RoomObservationScope, RoomObservationSubscription, Sha256Digest,
};
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey, SemanticDigest};
use waddle_xmpp::ownership::NodeIdentity;
use waddle_xmpp_core::xep0359::{add_origin_id, add_stanza_id, StanzaId};
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

use crate::ingress_substrate::EffectReceiptKind;
use crate::ingress_uow::{
    CanonicalMessageRepository, CapturedRoomSource, EffectIntentRepository, IngressUnitOfWork,
    IngressUowTransaction, ObservationWork, RoomObservationRepository,
};

/// Immutable input used to seed a single root-source observer work item.
#[derive(Clone)]
pub struct ObserverPilotSeed {
    key: MessageKey,
    observer: ConfiguredRoomObserver,
    intent: IngressEffectIntent,
    digest: SemanticDigest,
}

impl ObserverPilotSeed {
    pub fn message_key(&self) -> MessageKey {
        self.key
    }

    pub fn delivery_key(&self) -> waddle_xmpp::ingress::DeliveryKey {
        waddle_xmpp::ingress::DeliveryKey::effect(self.key, &self.intent.semantic_key())
    }
}

/// A reservation can be copied for a contention test; a callback grant cannot.
#[derive(Clone)]
pub struct ObserverPilotClaim {
    work: ObservationWork,
    observer: ConfiguredRoomObserver,
    intent: IngressEffectIntent,
    digest: SemanticDigest,
    effect_hash: [u8; 32],
    receipt: ObserverPilotReceiptIdentity,
}

impl ObserverPilotClaim {
    pub fn work_id(&self) -> uuid::Uuid {
        self.work.id
    }

    pub fn lease(&self) -> uuid::Uuid {
        self.work.lease
    }

    /// Corrupt a caller-held identity without changing the database.
    pub fn corrupt_for_test(&mut self, field: &str) -> anyhow::Result<()> {
        match field {
            "work" => self.work.id = uuid::Uuid::new_v4(),
            "token" => self.work.lease = uuid::Uuid::new_v4(),
            "body" => self.work.body = DisplayText::try_new("different body")?,
            "attempt" => self.work.attempt = self.work.attempt.saturating_add(1),
            "config" => {
                self.observer.max_concurrent = self.observer.max_concurrent.saturating_add(1)
            }
            "generation" => self.observer.generation = ObservationGeneration::try_new(2)?,
            "subscription_generation" => {
                self.work.subscription.generation = ObservationGeneration::try_new(2)?
            }
            "identity" => self.work.subscription.identity = Sha256Digest::try_new("b".repeat(64))?,
            "source" => {
                self.work.source.revision =
                    MessageRevision::new(self.work.source.revision.get() + 1)
            }
            "digest" => self.digest = SemanticDigest::from_storage(1, [9; 32])?,
            "hash" => self.effect_hash = [8; 32],
            "receipt_message" => self.receipt.message = MessageKey::new(),
            "receipt_kind" => self.receipt.kind = EffectReceiptKind::from_storage(0),
            "receipt_hash" => self.receipt.semantic_identity_hash = [7; 32],
            "kind" => {
                self.intent = IngressEffectIntent::RouteOccupantPm {
                    recipient: "room@conference.example.org/recipient".parse()?,
                    sender: "room@conference.example.org/author".parse()?,
                }
            }
            "payload" => {
                if let IngressEffectIntent::RoomObserver { requester, .. } = &mut self.intent {
                    *requester = "different@example.org".parse()?;
                }
            }
            "message" => self.work.message_key = MessageKey::new(),
            _ => anyhow::bail!("unknown fixture corruption"),
        }
        Ok(())
    }
}

// Kept private and derived from the verified work. An independently created
// receipt is not authority to invoke the callback or recover its result.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ObserverPilotReceiptIdentity {
    message: MessageKey,
    kind: EffectReceiptKind,
    semantic_identity_hash: [u8; 32],
}

fn receipt_identity(work: &ObservationWork) -> ObserverPilotReceiptIdentity {
    let effect = work.effect_key();
    ObserverPilotReceiptIdentity {
        message: work.message_key,
        kind: EffectReceiptKind::from_storage(effect.storage_kind()),
        semantic_identity_hash: Sha256::digest(effect.storage_identity().as_bytes()).into(),
    }
}

// The complete guard snapshot captures the work, lease, node incarnation,
// source, configuration, canonical message, delivery and effect at start.
// This is process-local evidence, not a durable result or an attestation.
struct ObserverPilotStartIdentity {
    snapshot: serde_json::Value,
    receipt: ObserverPilotReceiptIdentity,
}

impl ObserverPilotStartIdentity {
    fn verify(&self, claim: &ObserverPilotClaim) -> anyhow::Result<()> {
        ensure!(
            claim.receipt == self.receipt,
            "fixture receipt identity mismatch"
        );
        verify_snapshot(&self.snapshot, claim)
    }
}

/// A start whose transaction has not yet been committed. It carries no grant.
pub struct PreparedObserverPilotStart<'a> {
    tx: IngressUowTransaction<'a>,
    identity: ObserverPilotStartIdentity,
}

/// One acknowledged start. This value is intentionally neither `Clone` nor `Copy`.
pub struct ObserverPilotGrant {
    identity: ObserverPilotStartIdentity,
}

// Result custody is private, non-cloneable and tied to the acknowledged start.
// It exists only after the callback returned in this process.
struct ReturnedObserverPilotResult<T> {
    identity: ObserverPilotStartIdentity,
    value: T,
}

enum ObserverPilotState<T> {
    Ready,
    Invoking,
    Returned(ReturnedObserverPilotResult<T>),
}

/// Fixture-only state owned by the live caller, not recoverable after a crash.
pub struct ObserverPilotCustody<T> {
    identity: Option<ObserverPilotStartIdentity>,
    state: ObserverPilotState<T>,
}

impl PreparedObserverPilotStart<'_> {
    /// Only an acknowledged COMMIT creates a live-caller grant.
    pub async fn commit(self) -> anyhow::Result<ObserverPilotGrant> {
        self.tx.commit().await?;
        Ok(ObserverPilotGrant {
            identity: self.identity,
        })
    }
}

impl ObserverPilotGrant {
    /// Transfer the single-use grant into caller-owned fixture custody.
    pub fn into_custody<T>(self) -> ObserverPilotCustody<T> {
        ObserverPilotCustody {
            identity: Some(self.identity),
            state: ObserverPilotState::Ready,
        }
    }
}

impl<T> ObserverPilotCustody<T> {
    /// Enter the callback at most once. Dropping an unpolled future does not
    /// enter it; cancellation or panic after entry leaves the outcome unknown.
    pub async fn invoke<F, Fut>(
        &mut self,
        claim: &ObserverPilotClaim,
        callback: F,
    ) -> anyhow::Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        ensure!(
            matches!(self.state, ObserverPilotState::Ready),
            "fixture callback already entered"
        );
        self.identity
            .as_ref()
            .context("fixture start identity missing")?
            .verify(claim)?;
        self.state = ObserverPilotState::Invoking;
        let value = callback().await;
        // No await separates observing callback completion and taking custody.
        let identity = self
            .identity
            .take()
            .context("fixture start identity missing")?;
        self.state = ObserverPilotState::Returned(ReturnedObserverPilotResult { identity, value });
        Ok(())
    }

    /// Use the exact returned value for a fixture retry without invoking again.
    /// This rechecks the caller-held claim against the frozen start snapshot,
    /// not current database authority. It cannot authorize a production receipt
    /// or publication. A returned future may borrow the value; dropping it
    /// leaves custody here.
    pub fn retry<'a, F, R>(&'a self, claim: &ObserverPilotClaim, retry: F) -> anyhow::Result<R>
    where
        F: FnOnce(&'a T) -> R,
    {
        let ObserverPilotState::Returned(returned) = &self.state else {
            anyhow::bail!("fixture callback result unavailable");
        };
        returned.identity.verify(claim)?;
        Ok(retry(&returned.value))
    }
}

/// Reconstruct only the fixed fixture input for a separate process.
pub fn observer_pilot_seed(key: MessageKey) -> anyhow::Result<ObserverPilotSeed> {
    let room: jid::BareJid = "room@conference.example.org".parse()?;
    let sender: jid::BareJid = "author@example.org".parse()?;
    let observer = ConfiguredRoomObserver {
        plugin: PluginId::try_new("observer-quarantine-fixture")?,
        generation: ObservationGeneration::try_new(1)?,
        identity: Sha256Digest::try_new("a".repeat(64))?,
        scope: RoomObservationScope::Rooms(vec![room.clone()]),
        max_concurrent: 1,
    };
    let intent = IngressEffectIntent::RoomObserver {
        room,
        requester: sender,
        sender: "room@conference.example.org/author".parse()?,
        plugin: observer.plugin.clone(),
        generation: observer.generation,
        identity: observer.identity.clone(),
        correction_target: None,
    };
    Ok(ObserverPilotSeed {
        key,
        observer,
        intent,
        digest: SemanticDigest::from_storage(1, [4; 32])?,
    })
}

pub async fn seed_observer_pilot(
    uow: &IngressUnitOfWork,
    observed_at: DateTime<Utc>,
) -> anyhow::Result<ObserverPilotSeed> {
    let key = MessageKey::new();
    let seed = observer_pilot_seed(key)?;
    let room: jid::BareJid = "room@conference.example.org".parse()?;
    let sender: jid::BareJid = "author@example.org".parse()?;
    let observer = &seed.observer;
    let intent = &seed.intent;
    let mut message = Message::new(Some(room.clone().into()));
    message.from = Some("room@conference.example.org/author".parse()?);
    message.type_ = MessageType::Groupchat;
    message.id = Some(Id("observer-quarantine-wire".to_owned()));
    message
        .bodies
        .insert(Lang::new(), "fixture body".to_owned());
    add_stanza_id(
        &mut message,
        &StanzaId::new("observer-quarantine-stanza", room.clone().into()),
    );
    add_origin_id(&mut message, "observer-quarantine-origin");
    let mut tx = uow.begin().await?;
    CanonicalMessageRepository::record_message(&mut tx, key, &seed.digest, None).await?;
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(intent), false).await?;
    RoomObservationRepository::sync_configured(
        &mut tx,
        std::slice::from_ref(observer),
        observed_at.timestamp_millis(),
    )
    .await?;
    RoomObservationRepository::capture(
        &mut tx,
        CapturedRoomSource {
            key,
            room: &room,
            message: &message,
            sender: &sender,
            intents: std::slice::from_ref(intent),
            observed_at,
            correction_target: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(seed)
}

pub async fn claim_observer_pilot(
    uow: &IngressUnitOfWork,
    seed: &ObserverPilotSeed,
    now_ms: i64,
) -> anyhow::Result<Option<ObserverPilotClaim>> {
    let subscription = RoomObservationSubscription {
        plugin: seed.observer.plugin.clone(),
        generation: seed.observer.generation,
        identity: seed.observer.identity.clone(),
        room: "room@conference.example.org".parse()?,
    };
    let mut tx = uow.begin().await?;
    let work = RoomObservationRepository::claim(&mut tx, &subscription, now_ms).await?;
    tx.commit().await?;
    Ok(work.map(|work| {
        let receipt = receipt_identity(&work);
        let effect_hash = receipt.semantic_identity_hash;
        ObserverPilotClaim {
            work,
            observer: seed.observer.clone(),
            intent: seed.intent.clone(),
            digest: seed.digest.clone(),
            effect_hash,
            receipt,
        }
    }))
}

/// Reuse the real start checks and binding, then verify the fixture guard's
/// exact snapshot on the same transaction before permitting COMMIT.
pub async fn prepare_observer_pilot_start<'a>(
    uow: &'a IngressUnitOfWork,
    claim: &ObserverPilotClaim,
    now_ms: i64,
) -> anyhow::Result<Option<PreparedObserverPilotStart<'a>>> {
    let mut tx = uow.begin().await?;
    if !RoomObservationRepository::start(&mut tx, &claim.work, now_ms).await? {
        tx.commit().await?;
        return Ok(None);
    }
    let mut rows = tx
        .transaction_mut()
        .query(
            "SELECT status, dark_snapshot::text FROM extension_room_observation_work WHERE id = ?",
            crate::db_params![claim.work.id.to_string()],
        )
        .await?;
    let row = rows.next().await?.context("fixture work disappeared")?;
    let status: String = row.get(0)?;
    let snapshot: Option<String> = row.get(1)?;
    drop(rows);
    ensure!(
        status == "dark_started",
        "fixture guard did not quarantine the start"
    );
    let snapshot: serde_json::Value =
        serde_json::from_str(&snapshot.context("fixture snapshot missing")?)?;
    verify_snapshot(&snapshot, claim)?;
    Ok(Some(PreparedObserverPilotStart {
        tx,
        identity: ObserverPilotStartIdentity {
            snapshot,
            receipt: claim.receipt,
        },
    }))
}

fn verify_snapshot(snapshot: &serde_json::Value, claim: &ObserverPilotClaim) -> anyhow::Result<()> {
    let derived_receipt = receipt_identity(&claim.work);
    ensure!(
        claim.receipt == derived_receipt
            && claim.effect_hash == derived_receipt.semantic_identity_hash,
        "fixture receipt identity mismatch"
    );
    let work = &snapshot["work"];
    let observer = &snapshot["observer"];
    let source = &snapshot["source"];
    let canonical = &snapshot["canonical"];
    let delivery = &snapshot["delivery"];
    let effect = &snapshot["effect"];
    let local = NodeIdentity::local();
    let (digest_version, digest) = claim.digest.to_storage();
    let (kind, payload) = claim
        .intent
        .with_encoded_v1(|kind, payload| (kind, payload.to_vec()))?;
    ensure!(
        snapshot["version"] == 1
            && work["id"] == claim.work.id.to_string()
            && work["source_key"] == claim.work.message_key.to_storage().to_string()
            && work["message_key"] == claim.work.message_key.to_storage().to_string()
            && work["lease_id"] == claim.work.lease.to_string()
            && work["lease_node_id"] == local.node_id
            && work["lease_node_incarnation"] == local.node_epoch
            && work["attempt"] == claim.work.attempt
            && work["plugin_id"] == claim.work.subscription.plugin.as_str()
            && work["generation"] == claim.work.subscription.generation.get()
            && work["identity"] == claim.work.subscription.identity.as_str()
            && work["room_jid"] == claim.work.subscription.room.to_string()
            && work["revision"] == claim.work.source.revision.get()
            && work["body"] == claim.work.body.as_str()
            && serde_json::from_str::<serde_json::Value>(
                work["source_json"].as_str().context("work source")?
            )? == serde_json::to_value(&claim.work.source)?
            && source["source_key"] == work["source_key"]
            && source["revision"] == claim.work.source.revision.get()
            && source["retracted"] == 0
            && source["source_json"] == work["source_json"]
            && observer["plugin_id"] == claim.observer.plugin.as_str()
            && observer["generation"] == claim.observer.generation.get()
            && observer["identity"] == claim.observer.identity.as_str()
            && observer["max_concurrent"] == claim.observer.max_concurrent
            && serde_json::from_str::<serde_json::Value>(
                observer["scope_json"].as_str().context("observer scope")?
            )? == serde_json::to_value(&claim.observer.scope)?
            && canonical["message_key"] == claim.work.message_key.to_storage().to_string()
            && canonical["digest_version"] == digest_version
            && canonical["digest"] == hex::encode(digest)
            && delivery["message_key"] == claim.work.message_key.to_storage().to_string()
            && delivery["delivery_key"] == claim.work.delivery_key().to_storage().to_string()
            && effect["kind"] == kind
            && effect["kind"] == derived_receipt.kind.to_storage()
            && effect["identity_hash"] == hex::encode(claim.effect_hash)
            && effect["payload_version"] == 1
            && effect["payload"] == hex::encode(payload),
        "fixture start identity mismatch"
    );
    Ok(())
}
