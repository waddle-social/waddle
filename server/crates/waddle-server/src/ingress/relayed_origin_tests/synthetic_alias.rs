//! A storage-only model of sibling-session alias replay.
//!
//! A persisted principal reference is not an authentication credential. These
//! tests deliberately seed synthetic sessions and only model the database
//! checks; they do not establish possession of a live authenticated connection
//! or define an activation API. Production source and owner checks stay exact.

use jid::{BareJid, FullJid};
use uuid::Uuid;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ingress::{
    AliasOutcome, AliasResolution, DigestContext, DigestInput, IngressOrdinal, MessageKey,
    NormalizedTarget, SemanticDigest, SmIngressId, WireHandledCount,
};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp_core::xep0359::OriginId;

use super::groupchat;
use super::IngressFixture;
use crate::db::DatabaseDriver;
use crate::ingress_substrate::{CanonicalSourceBinding, IngressSubstrateError, MessageEnvelope};
use crate::ingress_uow::{
    CanonicalMessageRepository, EffectIntentRepository, IngressUowError, IngressUowTransaction,
    PrincipalAssertion, PrincipalRepository, SmIngressRepository, SmIngressStreamRepository,
};

/// A stored first source, deliberately distinct from a synthetic retry input.
/// It is not accepted as a retry principal by this model.
#[derive(Debug, PartialEq, Eq)]
struct FirstSourceAuthority {
    bare: BareJid,
    context: AuthContextId,
    context_version: AuthContextVersion,
    epoch: PrincipalAuthEpoch,
    occurrence: Option<(SmIngressId, IngressOrdinal)>,
}

#[derive(Debug, PartialEq, Eq)]
enum SyntheticAliasResolution {
    NewBound(MessageKey),
    ExistingFirstBound {
        key: MessageKey,
        first_source: FirstSourceAuthority,
    },
    LegacyUnbound(MessageKey),
    Conflict,
}

#[derive(Clone, Copy)]
struct SyntheticAliasOffer<'a> {
    sender: &'a BareJid,
    full_sender: &'a FullJid,
    target: &'a NormalizedTarget,
    origin: &'a OriginId,
    digest: &'a SemanticDigest,
    /// Synthetic input, supplied independently of the recorded first source.
    source: &'a CanonicalSourceBinding,
    /// A claimed replay must resolve this key; a missing alias is a conflict.
    expected_key: Option<MessageKey>,
}

async fn resolve_synthetic_alias(
    tx: &mut IngressUowTransaction<'_>,
    offer: SyntheticAliasOffer<'_>,
    mint: impl FnOnce() -> MessageKey,
) -> Result<SyntheticAliasResolution, IngressUowError> {
    if PrincipalRepository::assert_principal(tx, offer.source.principal()).await?
        != PrincipalAssertion::Asserted
    {
        return Err(IngressUowError::PrincipalAssertionFailed);
    }
    if offer.sender != offer.source.principal().bare_jid()
        || offer.full_sender.to_bare() != *offer.sender
    {
        return Ok(SyntheticAliasResolution::Conflict);
    }
    if let Some(expected) = offer.expected_key {
        match CanonicalMessageRepository::assert_relayed_alias(
            tx,
            expected,
            offer.sender,
            offer.target,
            offer.origin,
            offer.digest,
        )
        .await
        {
            Ok(()) => {}
            Err(IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)) => {
                return Ok(SyntheticAliasResolution::Conflict);
            }
            Err(error) => return Err(error),
        }
    }

    // The exact production resolver remains unchanged. If it rejects a
    // different synthetic context, re-resolve the already locked alias via
    // the ordinary identity/digest path solely for this private model.
    let (resolved, source_mismatch) =
        match CanonicalMessageRepository::resolve_and_record_alias_with_source(
            tx,
            offer.sender,
            offer.target,
            offer.origin,
            offer.digest,
            offer.source,
            mint,
        )
        .await
        {
            Ok(resolved) => (resolved, false),
            Err(IngressUowError::Substrate(IngressSubstrateError::SourceBindingConflict)) => (
                CanonicalMessageRepository::resolve_and_record_alias(
                    tx,
                    offer.sender,
                    offer.target,
                    offer.origin,
                    offer.digest,
                    MessageKey::new,
                )
                .await?,
                true,
            ),
            Err(error) => return Err(error),
        };
    match resolved {
        AliasResolution::Aliased(AliasOutcome::Inserted(key))
            if !source_mismatch && offer.expected_key.is_none() =>
        {
            Ok(SyntheticAliasResolution::NewBound(key))
        }
        AliasResolution::Aliased(AliasOutcome::Existing(key)) => {
            if offer.expected_key.is_some_and(|expected| expected != key) {
                return Ok(SyntheticAliasResolution::Conflict);
            }
            match load_first_source(tx, key).await? {
                Some(first_source) if &first_source.bare == offer.sender => {
                    Ok(SyntheticAliasResolution::ExistingFirstBound { key, first_source })
                }
                Some(_) => Ok(SyntheticAliasResolution::Conflict),
                None => Ok(SyntheticAliasResolution::LegacyUnbound(key)),
            }
        }
        AliasResolution::Aliased(AliasOutcome::Conflict(_)) => {
            Ok(SyntheticAliasResolution::Conflict)
        }
        _ => Err(IngressSubstrateError::AliasMissingAfterConflict.into()),
    }
}

async fn load_first_source(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
) -> Result<Option<FirstSourceAuthority>, IngressUowError> {
    let tx = tx.transaction_mut();
    let sql = if tx.driver() == DatabaseDriver::Postgres {
        "SELECT source_binding_version::int, source_bare_jid, source_auth_context_id::text, source_auth_context_version::text, source_auth_epoch::text, source_sm_ingress_id::text, source_ingress_ordinal::text FROM ingress_messages WHERE message_key = ?::uuid FOR UPDATE"
    } else {
        "SELECT source_binding_version, source_bare_jid, source_auth_context_id, source_auth_context_version, source_auth_epoch, source_sm_ingress_id, source_ingress_ordinal FROM ingress_messages WHERE message_key = ?"
    };
    let mut rows = tx
        .query(sql, crate::db_params![key.to_storage().to_string()])
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(IngressSubstrateError::RelayedAliasUnproven)?;
    let version: Option<i32> = row.get(0)?;
    let bare: Option<String> = row.get(1)?;
    let context: Option<String> = row.get(2)?;
    let context_version: Option<String> = row.get(3)?;
    let epoch: Option<String> = row.get(4)?;
    let sm: Option<String> = row.get(5)?;
    let ordinal: Option<String> = row.get(6)?;
    if version.is_none()
        && bare.is_none()
        && context.is_none()
        && context_version.is_none()
        && epoch.is_none()
        && sm.is_none()
        && ordinal.is_none()
    {
        return Ok(None);
    }
    let invalid = || IngressSubstrateError::SourceBindingConflict;
    let (Some(1), Some(bare), Some(context), Some(context_version), Some(epoch)) =
        (version, bare, context, context_version, epoch)
    else {
        return Err(invalid().into());
    };
    let bare = bare.parse().map_err(|_| invalid())?;
    let context = AuthContextId::new(context.parse().map_err(|_| invalid())?);
    let context_version = AuthContextVersion::new(context_version.parse().map_err(|_| invalid())?);
    let epoch = PrincipalAuthEpoch::new(epoch.parse().map_err(|_| invalid())?);
    let occurrence = match (sm, ordinal) {
        (None, None) => None,
        (Some(sm), Some(ordinal)) => Some((
            SmIngressId::from_storage(sm.parse().map_err(|_| invalid())?),
            IngressOrdinal::from_storage(ordinal.parse().map_err(|_| invalid())?)
                .map_err(|_| invalid())?,
        )),
        _ => return Err(invalid().into()),
    };
    Ok(Some(FirstSourceAuthority {
        bare,
        context,
        context_version,
        epoch,
        occurrence,
    }))
}

/// A new binding may write its first envelope. A replay validates only the
/// stored digest: it cannot fill an empty first envelope from a sibling.
async fn finish_synthetic_alias(
    tx: &mut IngressUowTransaction<'_>,
    outcome: &SyntheticAliasResolution,
    digest: &SemanticDigest,
    source: &CanonicalSourceBinding,
    envelope: &MessageEnvelope,
) -> Result<(), IngressUowError> {
    match outcome {
        SyntheticAliasResolution::NewBound(key) => {
            CanonicalMessageRepository::record_message_with_source(
                tx,
                *key,
                digest,
                Some(envelope),
                source,
            )
            .await
        }
        SyntheticAliasResolution::ExistingFirstBound { key, .. }
        | SyntheticAliasResolution::LegacyUnbound(key) => {
            CanonicalMessageRepository::record_message(tx, *key, digest, None).await
        }
        SyntheticAliasResolution::Conflict => {
            Err(IngressSubstrateError::RelayedAliasUnproven.into())
        }
    }
}

async fn add_synthetic_session(fixture: &IngressFixture, principal: &AuthenticatedPrincipalRef) {
    let id = Uuid::new_v4().to_string();
    if principal.bare_jid() != fixture.principal.bare_jid() {
        let localpart = format!("synthetic-{}", Uuid::new_v4().simple());
        fixture.execute(
            "INSERT INTO users (jid, username, xmpp_localpart, localpart_key, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
            crate::db_params![principal.bare_jid().to_string(), localpart.clone(), localpart.clone(), localpart, chrono::Utc::now().to_rfc3339(), chrono::Utc::now().to_rfc3339()],
        ).await;
    }
    fixture.execute(
        "INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        crate::db_params![id.clone(), principal.bare_jid().to_string(), id, principal.auth_context_id().as_uuid().to_string(), i64::try_from(principal.auth_context_version().get()).expect("version fits"), i64::try_from(principal.auth_epoch().get()).expect("epoch fits"), chrono::Utc::now().to_rfc3339(), chrono::Utc::now().to_rfc3339()],
    ).await;
}

async fn sibling_replay_model(fixture: IngressFixture) {
    let room: BareJid = "room@muc.example.com".parse().expect("room");
    let first = groupchat(fixture.submission(Some("synthetic-replay"), "body"), &room);
    let digest = waddle_xmpp::ingress::digest::v1::digest(&first.digest_input);
    let origin = first.digest_input.origin().expect("origin");
    let sender = first.sender.to_bare();
    let mut tx = fixture.uow.begin().await.expect("mint streams");
    let first_sm = SmIngressStreamRepository::mint(&mut tx, &SmSessionId::new("synthetic-first"))
        .await
        .expect("first stream");
    let retry_sm = SmIngressStreamRepository::mint(&mut tx, &SmSessionId::new("synthetic-retry"))
        .await
        .expect("retry stream");
    tx.commit().await.expect("commit streams");
    let first_binding = CanonicalSourceBinding::new(
        fixture.principal.clone(),
        Some((first_sm, IngressOrdinal::FIRST)),
    );
    let first_offer = SyntheticAliasOffer {
        sender: &sender,
        full_sender: &first.sender,
        target: &first.target,
        origin,
        digest: &digest,
        source: &first_binding,
        expected_key: None,
    };
    let key = MessageKey::new();
    let envelope = MessageEnvelope::new(first.plan.sanitized_message.clone());
    let mut tx = fixture.uow.begin().await.expect("first binding");
    let inserted = resolve_synthetic_alias(&mut tx, first_offer, || key)
        .await
        .expect("new alias");
    assert_eq!(inserted, SyntheticAliasResolution::NewBound(key));
    finish_synthetic_alias(&mut tx, &inserted, &digest, &first_binding, &envelope)
        .await
        .expect("first envelope");
    EffectIntentRepository::reconcile(&mut tx, key, &first.plan.intents, false)
        .await
        .expect("first dispatch");
    CanonicalMessageRepository::initialize_room_owner_decision(&mut tx, key, &room)
        .await
        .expect("owner latch");
    SmIngressRepository::insert_sm_ref(
        &mut tx,
        first_sm,
        IngressOrdinal::FIRST,
        WireHandledCount::from_storage(1),
        key,
    )
    .await
    .expect("first SM reference");
    tx.commit().await.expect("commit first source");

    let retry_principal = AuthenticatedPrincipalRef::new(
        sender.clone(),
        AuthContextId::new(Uuid::new_v4()),
        AuthContextVersion::new(7),
        PrincipalAuthEpoch::new(9),
    );
    add_synthetic_session(&fixture, &retry_principal).await;
    let retry_binding =
        CanonicalSourceBinding::new(retry_principal, Some((retry_sm, IngressOrdinal::FIRST)));
    let retry_sender: FullJid = "romeo@example.com/tablet"
        .parse()
        .expect("sibling full sender");
    let mut retry_message = first.plan.sanitized_message.clone();
    retry_message.from = Some(retry_sender.clone().into());
    let retry_digest = waddle_xmpp::ingress::digest::v1::digest(
        &DigestInput::from_parsed(
            &retry_message,
            &DigestContext {
                target: first.target.clone(),
                server_authorities: vec![sender.clone(), room.clone()],
                stanza_lang: None,
            },
        )
        .expect("retry digest"),
    );
    assert_eq!(retry_digest, digest);
    let retry_offer = SyntheticAliasOffer {
        full_sender: &retry_sender,
        source: &retry_binding,
        expected_key: Some(key),
        ..first_offer
    };

    let mut exact = fixture.uow.begin().await.expect("exact source check");
    assert!(matches!(
        CanonicalMessageRepository::record_message_with_source(
            &mut exact,
            key,
            &digest,
            None,
            &retry_binding
        )
        .await,
        Err(IngressUowError::Substrate(
            IngressSubstrateError::SourceBindingConflict
        ))
    ));
    drop(exact);
    let mut tx = fixture.uow.begin().await.expect("synthetic sibling retry");
    let replay = resolve_synthetic_alias(&mut tx, retry_offer, MessageKey::new)
        .await
        .expect("model replay");
    assert_eq!(
        replay,
        SyntheticAliasResolution::ExistingFirstBound {
            key,
            first_source: FirstSourceAuthority {
                bare: fixture.principal.bare_jid().clone(),
                context: fixture.principal.auth_context_id().clone(),
                context_version: fixture.principal.auth_context_version(),
                epoch: fixture.principal.auth_epoch(),
                occurrence: Some((first_sm, IngressOrdinal::FIRST)),
            }
        }
    );
    finish_synthetic_alias(
        &mut tx,
        &replay,
        &digest,
        &retry_binding,
        &MessageEnvelope::new(retry_message),
    )
    .await
    .expect("replay preserves original envelope");
    SmIngressRepository::insert_sm_ref(
        &mut tx,
        retry_sm,
        IngressOrdinal::FIRST,
        WireHandledCount::from_storage(1),
        key,
    )
    .await
    .expect("retry SM reference");
    assert_eq!(
        CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("load original")
            .expect("original envelope")
            .message(),
        envelope.message()
    );
    assert_eq!(
        SmIngressRepository::lookup(&mut tx, first_sm, IngressOrdinal::FIRST)
            .await
            .expect("first ref"),
        Some(key)
    );
    assert_eq!(
        SmIngressRepository::lookup(&mut tx, retry_sm, IngressOrdinal::FIRST)
            .await
            .expect("retry ref"),
        Some(key)
    );
    assert!(CanonicalMessageRepository::assert_relayed_source(
        &mut tx,
        key,
        &first.sender,
        &room,
        &digest,
        &first_binding,
        None
    )
    .await
    .is_ok());
    assert!(matches!(
        CanonicalMessageRepository::assert_relayed_source(
            &mut tx,
            key,
            &retry_sender,
            &room,
            &digest,
            &retry_binding,
            None
        )
        .await,
        Err(IngressUowError::Substrate(
            IngressSubstrateError::RelayedAliasUnproven
        ))
    ));
    tx.commit().await.expect("commit retry reference");

    // Even if a first transaction left a bound row without an envelope, a
    // sibling's different full sender must not become that first envelope.
    let empty_origin = OriginId::new("synthetic-empty-envelope".to_owned());
    let empty_key = MessageKey::new();
    let mut tx = fixture.uow.begin().await.expect("empty first envelope");
    assert_eq!(
        resolve_synthetic_alias(
            &mut tx,
            SyntheticAliasOffer {
                origin: &empty_origin,
                ..first_offer
            },
            || empty_key,
        )
        .await
        .expect("bind empty first source"),
        SyntheticAliasResolution::NewBound(empty_key)
    );
    tx.commit().await.expect("commit empty first source");
    let mut tx = fixture.uow.begin().await.expect("retry empty envelope");
    let empty_replay = resolve_synthetic_alias(
        &mut tx,
        SyntheticAliasOffer {
            origin: &empty_origin,
            expected_key: Some(empty_key),
            ..retry_offer
        },
        MessageKey::new,
    )
    .await
    .expect("resolve empty first source");
    assert!(matches!(
        empty_replay,
        SyntheticAliasResolution::ExistingFirstBound { key, .. } if key == empty_key
    ));
    let mut sibling_envelope = first.plan.sanitized_message.clone();
    sibling_envelope.from = Some(retry_sender.clone().into());
    finish_synthetic_alias(
        &mut tx,
        &empty_replay,
        &digest,
        &retry_binding,
        &MessageEnvelope::new(sibling_envelope),
    )
    .await
    .expect("do not backfill the first envelope");
    assert!(
        CanonicalMessageRepository::load_envelope(&mut tx, empty_key)
            .await
            .expect("load empty envelope")
            .is_none()
    );
    tx.commit().await.expect("commit empty-envelope replay");

    let other_target = NormalizedTarget::Bare("other@muc.example.com".parse().expect("other room"));
    let other_origin = OriginId::new("different-origin".to_owned());
    let different = groupchat(
        fixture.submission(Some("synthetic-replay"), "different"),
        &room,
    );
    let other_digest = waddle_xmpp::ingress::digest::v1::digest(&different.digest_input);
    let other_bare: BareJid = "juliet@example.com".parse().expect("other sender");
    let other_full: FullJid = "juliet@example.com/tablet"
        .parse()
        .expect("other full sender");
    let other_principal = AuthenticatedPrincipalRef::new(
        other_bare.clone(),
        AuthContextId::new(Uuid::new_v4()),
        AuthContextVersion::new(1),
        PrincipalAuthEpoch::new(1),
    );
    add_synthetic_session(&fixture, &other_principal).await;
    let other_binding = CanonicalSourceBinding::new(other_principal, None);
    for offer in [
        SyntheticAliasOffer {
            target: &other_target,
            ..retry_offer
        },
        SyntheticAliasOffer {
            origin: &other_origin,
            ..retry_offer
        },
        SyntheticAliasOffer {
            digest: &other_digest,
            ..retry_offer
        },
        SyntheticAliasOffer {
            sender: &other_bare,
            ..retry_offer
        },
        SyntheticAliasOffer {
            full_sender: &other_full,
            ..retry_offer
        },
        SyntheticAliasOffer {
            source: &other_binding,
            ..retry_offer
        },
    ] {
        let mut tx = fixture.uow.begin().await.expect("conflicting retry");
        assert_eq!(
            resolve_synthetic_alias(&mut tx, offer, MessageKey::new)
                .await
                .expect("classify conflict"),
            SyntheticAliasResolution::Conflict
        );
        tx.commit().await.expect("finish conflict");
    }
    let missing_principal = AuthenticatedPrincipalRef::new(
        sender.clone(),
        AuthContextId::new(Uuid::new_v4()),
        AuthContextVersion::new(1),
        PrincipalAuthEpoch::new(1),
    );
    let missing_binding = CanonicalSourceBinding::new(missing_principal, None);
    let mut tx = fixture.uow.begin().await.expect("missing principal");
    assert!(matches!(
        resolve_synthetic_alias(
            &mut tx,
            SyntheticAliasOffer {
                source: &missing_binding,
                ..retry_offer
            },
            MessageKey::new
        )
        .await,
        Err(IngressUowError::PrincipalAssertionFailed)
    ));
    drop(tx);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 2);
    fixture.close().await;
}

async fn legacy_replay_model(fixture: IngressFixture) {
    let room: BareJid = "room@muc.example.com".parse().expect("room");
    let legacy = groupchat(
        fixture.submission(Some("synthetic-legacy"), "legacy"),
        &room,
    );
    let sender = legacy.sender.to_bare();
    let origin = legacy.digest_input.origin().expect("origin");
    let digest = waddle_xmpp::ingress::digest::v1::digest(&legacy.digest_input);
    let binding = CanonicalSourceBinding::new(fixture.principal.clone(), None);
    let key = MessageKey::new();
    let envelope = MessageEnvelope::new(legacy.plan.sanitized_message.clone());
    let mut tx = fixture.uow.begin().await.expect("legacy insert");
    assert!(
        matches!(CanonicalMessageRepository::resolve_and_record_alias(&mut tx, &sender, &legacy.target, origin, &digest, || key).await.expect("insert legacy"), AliasResolution::Aliased(AliasOutcome::Inserted(inserted)) if inserted == key)
    );
    CanonicalMessageRepository::record_message(&mut tx, key, &digest, Some(&envelope))
        .await
        .expect("legacy envelope");
    tx.commit().await.expect("commit legacy");
    let offer = SyntheticAliasOffer {
        sender: &sender,
        full_sender: &legacy.sender,
        target: &legacy.target,
        origin,
        digest: &digest,
        source: &binding,
        expected_key: Some(key),
    };
    let mut tx = fixture.uow.begin().await.expect("legacy replay");
    let replay = resolve_synthetic_alias(&mut tx, offer, MessageKey::new)
        .await
        .expect("classify legacy");
    assert_eq!(replay, SyntheticAliasResolution::LegacyUnbound(key));
    finish_synthetic_alias(&mut tx, &replay, &digest, &binding, &envelope)
        .await
        .expect("finish legacy replay");
    assert!(load_first_source(&mut tx, key)
        .await
        .expect("read legacy binding")
        .is_none());
    assert!(matches!(
        CanonicalMessageRepository::assert_relayed_source(
            &mut tx,
            key,
            &legacy.sender,
            &room,
            &digest,
            &binding,
            None
        )
        .await,
        Err(IngressUowError::Substrate(
            IngressSubstrateError::RelayedAliasUnproven
        ))
    ));
    tx.commit().await.expect("commit legacy replay");
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_synthetic_sibling_alias_replay() {
    sibling_replay_model(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn sqlite_synthetic_legacy_alias_replay() {
    legacy_replay_model(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_synthetic_sibling_alias_replay() {
    if let Some(fixture) = IngressFixture::postgres("synthetic_sibling_alias").await {
        sibling_replay_model(fixture).await;
    }
}

#[tokio::test]
async fn postgres_synthetic_legacy_alias_replay() {
    if let Some(fixture) = IngressFixture::postgres("synthetic_legacy_alias").await {
        legacy_replay_model(fixture).await;
    }
}
