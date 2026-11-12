//! Fixture-only, source-aware alias classification. This is not admission.
//!
//! The runtime must follow the alias protocol: no direct alias rewrite/delete,
//! and privileged GC must lock the canonical message before removing an alias.
//! A non-cooperating privileged writer is not fenced by this experiment.

use crate::ingress_substrate::EnteredTransaction;
use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ingress::{
    IngressOrdinal, MessageKey, NormalizedTarget, SemanticDigest, SmIngressId,
};
use waddle_xmpp_core::xep0359::OriginId;

use super::substrate::{
    decode_stored_alias, discard_database_error, insert_message, prepared_ingress_relation,
    validate_stored_alias_identity, validate_stored_alias_identity_at, AliasStorageKey,
    IngressSubstrateError, PreparedAttestedReceiptWriter, PreparedIngressRelation,
};
use super::{authority, CanonicalSourceBinding, MessageEnvelope};
use crate::db::{DatabaseDriver, Row};
use crate::ingress_uow::{
    IngressUowError, IngressUowTransaction, PrincipalAssertion, PrincipalRepository,
};

pub(crate) struct SourceAliasOffer<'a> {
    pub sender: &'a FullJid,
    pub target: &'a NormalizedTarget,
    pub origin: &'a OriginId,
    pub digest: &'a SemanticDigest,
    pub source: &'a CanonicalSourceBinding,
    pub envelope: &'a MessageEnvelope,
}

/// Provisional until the returned UOW commits. First source is stored data, not
/// a credential for the current offer or evidence of live authentication.
/// ExistingBound establishes only the source binding: historical envelopes may
/// be absent or malformed, and are never filled or validated by this classifier.
/// An optional SM ID/ordinal is recorded input: this fixture does not assert it
/// against an authenticated stream/session or establish occurrence authority.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SourceAliasClassification {
    NewBound(MessageKey),
    ExistingBound {
        key: MessageKey,
        first_source: CanonicalSourceBinding,
    },
    LegacyUnbound(MessageKey),
    Conflict,
}

#[derive(Debug, Error)]
pub(crate) enum SourceAliasError {
    #[error(transparent)]
    Uow(#[from] IngressUowError),
    #[error(transparent)]
    Substrate(#[from] IngressSubstrateError),
    #[error("source-alias laboratory requires a prepared PostgreSQL transaction")]
    Unprepared,
    #[error("locked alias disappeared, changed identity, or rebound during classification")]
    AliasChanged,
    #[error("stored first source binding is malformed")]
    MalformedSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SavepointPhase {
    Begin,
    Rollback,
    Release,
}

tokio::task_local! {
    /// Tests emulate a command that reached the server but whose completion
    /// has not returned; the enclosing UOW must be dropped on cancellation.
    pub(crate) static STALL_SAVEPOINT_PHASE: SavepointPhase;
    /// Hold a stale alias probe before acquiring its canonical row lock.
    pub(crate) static STALL_AFTER_PROBE: ();
}

async fn savepoint_command(
    tx: &mut EnteredTransaction<'_>,
    phase: SavepointPhase,
) -> Result<(), IngressSubstrateError> {
    let statement = match phase {
        SavepointPhase::Begin => "SAVEPOINT waddle_source_alias_candidate",
        SavepointPhase::Rollback => "ROLLBACK TO SAVEPOINT waddle_source_alias_candidate",
        SavepointPhase::Release => "RELEASE SAVEPOINT waddle_source_alias_candidate",
    };
    let stalled = STALL_SAVEPOINT_PHASE
        .try_with(|selected| *selected == phase)
        .unwrap_or(false);
    if stalled && tx.driver() == DatabaseDriver::Postgres {
        // raw_sql sends the fixed pair as one protocol operation. The test
        // cancels while the second command is sleeping, after the savepoint
        // command has executed but before the client has its completion.
        let sql = format!("{statement}; SELECT pg_catalog.pg_sleep(3)");
        tx.execute_batch(&sql)
            .await
            .map_err(discard_database_error)?;
    } else {
        tx.execute(statement, ())
            .await
            .map_err(discard_database_error)?;
    }
    Ok(())
}

const INSERT_ALIAS_MAIN_SQLITE: &str = "INSERT INTO main.ingress_origin_aliases (alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (alias_key_hash) DO NOTHING";

async fn probe_alias(
    tx: &mut EnteredTransaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    alias: &AliasStorageKey<'_>,
) -> Result<Option<MessageKey>, IngressSubstrateError> {
    let pg = format!(
        "SELECT message_key::text, sender_bare_jid, target_kind, target_jid, origin_id FROM {} WHERE alias_key_hash = ?",
        prepared_ingress_relation(prepared, PreparedIngressRelation::Aliases),
    );
    const SQLITE: &str = "SELECT message_key, sender_bare_jid, target_kind, target_jid, origin_id FROM main.ingress_origin_aliases WHERE alias_key_hash = ?";
    let mut rows = tx
        .query(
            if tx.driver() == DatabaseDriver::Postgres {
                &pg
            } else {
                SQLITE
            },
            crate::db_params![alias.hash.to_vec()],
        )
        .await
        .map_err(discard_database_error)?;
    let Some(row) = rows.next().await.map_err(discard_database_error)? else {
        return Ok(None);
    };
    validate_stored_alias_identity_at(&row, alias, 1)?;
    let key: String = row.get(0).map_err(discard_database_error)?;
    key.parse::<Uuid>()
        .map(MessageKey::from_storage)
        .map(Some)
        .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)
}

async fn lock_probed_message(
    tx: &mut EnteredTransaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    key: MessageKey,
) -> Result<bool, IngressSubstrateError> {
    let pg = format!(
        "SELECT 1 FROM {} WHERE message_key = ?::uuid FOR UPDATE",
        prepared_ingress_relation(prepared, PreparedIngressRelation::Messages),
    );
    let mut rows = tx
        .query(
            if tx.driver() == DatabaseDriver::Postgres {
                &pg
            } else {
                "SELECT 1 FROM main.ingress_messages WHERE message_key = ?"
            },
            crate::db_params![key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    Ok(rows.next().await.map_err(discard_database_error)?.is_some())
}

async fn insert_source_candidate(
    tx: &mut EnteredTransaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
    digest: &SemanticDigest,
    envelope: Option<&MessageEnvelope>,
    source: Option<&CanonicalSourceBinding>,
) -> Result<bool, IngressSubstrateError> {
    if tx.driver() == DatabaseDriver::Postgres {
        return insert_message(tx, prepared, message_key, digest, envelope, source).await;
    }
    let (version, bytes) = digest.to_storage();
    let inserted = tx.execute(
        "INSERT INTO main.ingress_messages (message_key, digest_version, digest, envelope_version, envelope, source_binding_version, source_bare_jid, source_auth_context_id, source_auth_context_version, source_auth_epoch, source_sm_ingress_id, source_ingress_ordinal) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (message_key) DO NOTHING",
        crate::db_params![
            message_key.to_storage().to_string(), i32::from(version), bytes.to_vec(),
            envelope.map(|value| value.version().to_storage()),
            envelope.map(authority::serialize_envelope).transpose()?,
            source.map(|_| 1_i32),
            source.map(|value| value.principal().bare_jid().to_string()),
            source.map(|value| value.principal().auth_context_id().as_uuid().to_string()),
            source.map(|value| value.principal().auth_context_version().get().to_string()),
            source.map(|value| value.principal().auth_epoch().get().to_string()),
            source.and_then(|value| value.sm_occurrence().map(|(id, _)| id.to_storage().to_string())),
            source.and_then(|value| value.sm_occurrence().map(|(_, ordinal)| ordinal.to_storage().to_string())),
        ],
    ).await.map_err(discard_database_error)?;
    Ok(inserted == 1)
}

/// Fixture-only storage operation. The caller has already asserted the current
/// principal and owns the complete UOW; every error must drop that UOW.
async fn classify_after_assertion(
    tx: &mut EnteredTransaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    sender: &BareJid,
    offer: SourceAliasOffer<'_>,
    mint: impl FnOnce() -> MessageKey,
) -> Result<SourceAliasClassification, SourceAliasError> {
    let alias = AliasStorageKey::new(sender, offer.target, offer.origin);
    Ok(match probe_alias(tx, prepared, &alias).await? {
        Some(key) => {
            if STALL_AFTER_PROBE.try_with(|_| ()).is_ok() && tx.driver() == DatabaseDriver::Postgres
            {
                tx.execute("SELECT pg_catalog.pg_sleep(3)", ())
                    .await
                    .map_err(discard_database_error)?;
            }
            if !lock_probed_message(tx, prepared, key).await? {
                return Err(SourceAliasError::AliasChanged);
            }
            classify_locked(tx, prepared, &alias, key, offer.digest).await?
        }
        None => {
            let candidate = mint();
            savepoint_command(tx, SavepointPhase::Begin).await?;
            if !insert_source_candidate(
                tx,
                prepared,
                candidate,
                offer.digest,
                Some(offer.envelope),
                Some(offer.source),
            )
            .await?
            {
                return Err(IngressSubstrateError::MessageContentConflict.into());
            }
            let postgres = format!(
                    "INSERT INTO {} (alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key) VALUES (?, ?, ?, ?, ?, ?::uuid) ON CONFLICT (alias_key_hash) DO NOTHING",
                    prepared_ingress_relation(prepared, PreparedIngressRelation::Aliases)
                );
            let inserted = tx
                .execute(
                    if tx.driver() == DatabaseDriver::Postgres {
                        &postgres
                    } else {
                        INSERT_ALIAS_MAIN_SQLITE
                    },
                    crate::db_params![
                        alias.hash.to_vec(),
                        alias.sender.to_string(),
                        alias.target.kind(),
                        alias.target.jid(),
                        alias.origin_id.as_str(),
                        candidate.to_storage().to_string(),
                    ],
                )
                .await
                .map_err(discard_database_error)?;
            if inserted == 1 {
                savepoint_command(tx, SavepointPhase::Release).await?;
                SourceAliasClassification::NewBound(candidate)
            } else {
                savepoint_command(tx, SavepointPhase::Rollback).await?;
                savepoint_command(tx, SavepointPhase::Release).await?;
                let key = probe_alias(tx, prepared, &alias)
                    .await?
                    .ok_or(SourceAliasError::AliasChanged)?;
                if !lock_probed_message(tx, prepared, key).await? {
                    return Err(SourceAliasError::AliasChanged);
                }
                classify_locked(tx, prepared, &alias, key, offer.digest).await?
            }
        }
    })
}

async fn classify_locked(
    tx: &mut EnteredTransaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    alias: &AliasStorageKey<'_>,
    locked_key: MessageKey,
    offered_digest: &SemanticDigest,
) -> Result<SourceAliasClassification, SourceAliasError> {
    // The initial lookup locked only the canonical row, in the established GC
    // order. Freshly re-read the exact hash and key under READ COMMITTED; do not
    // lock or chase another canonical key if a non-cooperating writer rebounded it.
    let pg = format!(
        "SELECT m.message_key::text, m.digest_version, m.digest, a.sender_bare_jid, a.target_kind, a.target_jid, a.origin_id, m.source_binding_version::int, m.source_bare_jid, m.source_auth_context_id::text, m.source_auth_context_version::text, m.source_auth_epoch::text, m.source_sm_ingress_id::text, m.source_ingress_ordinal::text, m.owner_decision::int FROM {} a JOIN {} m USING (message_key) WHERE a.alias_key_hash = ? AND m.message_key = ?::uuid",
        prepared_ingress_relation(prepared, PreparedIngressRelation::Aliases),
        prepared_ingress_relation(prepared, PreparedIngressRelation::Messages),
    );
    const SQLITE: &str = "SELECT m.message_key, m.digest_version, m.digest, a.sender_bare_jid, a.target_kind, a.target_jid, a.origin_id, m.source_binding_version, m.source_bare_jid, m.source_auth_context_id, m.source_auth_context_version, m.source_auth_epoch, m.source_sm_ingress_id, m.source_ingress_ordinal, m.owner_decision FROM main.ingress_origin_aliases a JOIN main.ingress_messages m USING (message_key) WHERE a.alias_key_hash = ? AND m.message_key = ?";
    let mut rows = tx
        .query(
            if tx.driver() == DatabaseDriver::Postgres {
                &pg
            } else {
                SQLITE
            },
            crate::db_params![alias.hash.to_vec(), locked_key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    let row = rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(SourceAliasError::AliasChanged)?;
    validate_stored_alias_identity(&row, alias).map_err(|_| SourceAliasError::AliasChanged)?;
    let current = decode_stored_alias(&row)?;
    if current.key != locked_key {
        return Err(SourceAliasError::AliasChanged);
    }
    let first_source = decode_source(&row)?;
    if first_source
        .as_ref()
        .is_some_and(|source| source.principal().bare_jid() != alias.sender)
    {
        return Err(SourceAliasError::MalformedSource);
    }
    if &current.digest != offered_digest {
        return Ok(SourceAliasClassification::Conflict);
    }
    match first_source {
        Some(first_source) => Ok(SourceAliasClassification::ExistingBound {
            key: current.key,
            first_source,
        }),
        None => Ok(SourceAliasClassification::LegacyUnbound(current.key)),
    }
}

fn decode_source(row: &Row) -> Result<Option<CanonicalSourceBinding>, SourceAliasError> {
    let version: Option<i32> = row.get(7).map_err(discard_database_error)?;
    let bare: Option<String> = row.get(8).map_err(discard_database_error)?;
    let context: Option<String> = row.get(9).map_err(discard_database_error)?;
    let context_version: Option<String> = row.get(10).map_err(discard_database_error)?;
    let epoch: Option<String> = row.get(11).map_err(discard_database_error)?;
    let sm: Option<String> = row.get(12).map_err(discard_database_error)?;
    let ordinal: Option<String> = row.get(13).map_err(discard_database_error)?;
    let owner_decision: Option<i32> = row.get(14).map_err(discard_database_error)?;
    if version.is_none()
        && bare.is_none()
        && context.is_none()
        && context_version.is_none()
        && epoch.is_none()
        && sm.is_none()
        && ordinal.is_none()
    {
        return if owner_decision.is_none() {
            Ok(None)
        } else {
            Err(SourceAliasError::MalformedSource)
        };
    }
    let (Some(1), Some(bare), Some(context), Some(context_version), Some(epoch)) =
        (version, bare, context, context_version, epoch)
    else {
        return Err(SourceAliasError::MalformedSource);
    };
    if owner_decision.is_some_and(|decision| !(0..=2).contains(&decision)) {
        return Err(SourceAliasError::MalformedSource);
    }
    let bare = bare
        .parse()
        .map_err(|_| SourceAliasError::MalformedSource)?;
    let context = context
        .parse::<Uuid>()
        .map_err(|_| SourceAliasError::MalformedSource)?;
    let context_version: u64 = context_version
        .parse()
        .map_err(|_| SourceAliasError::MalformedSource)?;
    let epoch: u64 = epoch
        .parse()
        .map_err(|_| SourceAliasError::MalformedSource)?;
    if context_version == 0 || epoch == 0 {
        return Err(SourceAliasError::MalformedSource);
    }
    let occurrence = match (sm, ordinal) {
        (None, None) => None,
        (Some(sm), Some(ordinal)) => Some((
            SmIngressId::from_storage(sm.parse().map_err(|_| SourceAliasError::MalformedSource)?),
            IngressOrdinal::from_storage(
                ordinal
                    .parse()
                    .map_err(|_| SourceAliasError::MalformedSource)?,
            )
            .map_err(|_| SourceAliasError::MalformedSource)?,
        )),
        _ => return Err(SourceAliasError::MalformedSource),
    };
    Ok(Some(CanonicalSourceBinding::new(
        AuthenticatedPrincipalRef::new(
            bare,
            AuthContextId::new(context),
            AuthContextVersion::new(context_version),
            PrincipalAuthEpoch::new(epoch),
        ),
        occurrence,
    )))
}

/// Own the UOW before the first await. An error or cancellation drops it,
/// including an in-flight savepoint, so a partial candidate cannot be committed.
/// The durable principal is checked only at assertion time; this fixture does
/// not establish live authentication, caller/planner provenance, a claimed
/// relay key, source-frontier advancement, or automatic retry authority.
pub(crate) async fn classify_source_alias<'a>(
    mut transaction: IngressUowTransaction<'a>,
    offer: SourceAliasOffer<'_>,
    mint: impl FnOnce() -> MessageKey,
) -> Result<(IngressUowTransaction<'a>, SourceAliasClassification), SourceAliasError> {
    let driver = transaction.transaction_mut().driver();
    if driver == DatabaseDriver::Postgres && transaction.receipt_schema().is_none() {
        return Err(SourceAliasError::Unprepared);
    }
    if driver == DatabaseDriver::Sqlite {
        assert_sqlite_main_principal(&mut transaction, offer.source.principal()).await?;
    } else if PrincipalRepository::assert_principal(&mut transaction, offer.source.principal())
        .await?
        != PrincipalAssertion::Asserted
    {
        return Err(IngressUowError::PrincipalAssertionFailed.into());
    }
    if offer.source.principal().auth_context_version().get() == 0
        || offer.source.principal().auth_epoch().get() == 0
    {
        return Err(SourceAliasError::MalformedSource);
    }
    let sender = offer.sender.to_bare();
    if &sender != offer.source.principal().bare_jid() {
        return Ok((transaction, SourceAliasClassification::Conflict));
    }
    let result = {
        let (tx, prepared) = transaction.source_alias_parts_for_fixture();
        classify_after_assertion(tx, prepared, &sender, offer, mint).await?
    };
    Ok((transaction, result))
}

/// SQLite's temp schema precedes `main`; neither principal nor epoch evidence
/// may be taken from a shadow relation on a pooled connection.
async fn assert_sqlite_main_principal(
    transaction: &mut IngressUowTransaction<'_>,
    principal: &waddle_xmpp::auth::AuthenticatedPrincipalRef,
) -> Result<(), IngressUowError> {
    let expected_epoch = i64::from(transaction.protocol_epoch().to_storage());
    let mut rows = transaction
        .transaction_mut()
        .query(
            "SELECT epoch FROM main.ingress_protocol_epoch WHERE id = 1",
            (),
        )
        .await?;
    let actual_epoch: i64 = rows
        .next()
        .await?
        .ok_or(IngressUowError::EpochProofMissing)?
        .get(0)?;
    drop(rows);
    if actual_epoch != expected_epoch {
        return Err(IngressUowError::EpochProofMissing);
    }
    let mut rows = transaction.transaction_mut().query(
        "SELECT expires_at FROM main.sessions WHERE user_jid = ? AND auth_context_id = ? AND auth_context_version = ? AND principal_auth_epoch = ?",
        crate::db_params![
            principal.bare_jid().to_string(),
            principal.auth_context_id().as_uuid().to_string(),
            i64::try_from(principal.auth_context_version().get()).map_err(|_| IngressUowError::PrincipalReferenceOutOfRange)?,
            i64::try_from(principal.auth_epoch().get()).map_err(|_| IngressUowError::PrincipalReferenceOutOfRange)?,
        ],
    ).await?;
    let Some(row) = rows.next().await? else {
        return Err(IngressUowError::PrincipalAssertionFailed);
    };
    let expiry: Option<String> = row.get(0)?;
    let expired = expiry
        .map(|raw| {
            DateTime::parse_from_rfc3339(&raw)
                .map(|expires| Utc::now() >= expires.with_timezone(&Utc))
                .map_err(|_| IngressUowError::InvalidStoredPrincipalExpiry)
        })
        .transpose()?
        .unwrap_or(false);
    if expired {
        return Err(IngressUowError::PrincipalAssertionFailed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        classify_source_alias, SourceAliasClassification, SourceAliasError, SourceAliasOffer,
    };
    use crate::ingress::test_support::IngressFixture;
    use crate::ingress_substrate::{CanonicalSourceBinding, MessageEnvelope};
    use crate::ingress_uow::{
        CanonicalMessageRepository, SmIngressRepository, SmIngressStreamRepository,
    };
    use uuid::Uuid;
    use waddle_xmpp::auth::{
        AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
    };
    use waddle_xmpp::ingress::{
        AliasOutcome, AliasResolution, IngressOrdinal, MessageKey, WireHandledCount,
    };
    use waddle_xmpp::pending_delivery::SmSessionId;

    async fn add_session(fixture: &IngressFixture, principal: &AuthenticatedPrincipalRef) {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        fixture.execute(
            "INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            crate::db_params![id.clone(), principal.bare_jid().to_string(), id, principal.auth_context_id().as_uuid().to_string(), i64::try_from(principal.auth_context_version().get()).expect("version"), i64::try_from(principal.auth_epoch().get()).expect("epoch"), now.clone(), now],
        ).await;
    }

    #[tokio::test]
    async fn sqlite_first_sibling_conflict_and_rollback() {
        let fixture = IngressFixture::sqlite().await;
        let first = fixture.submission(Some("source-alias"), "first body");
        let digest = waddle_xmpp::ingress::digest::v1::digest(&first.digest_input);
        let origin = first.digest_input.origin().expect("origin");
        let first_envelope = MessageEnvelope::new(first.plan.sanitized_message.clone());
        let mut streams = fixture.uow.begin().await.expect("streams");
        let first_sm =
            SmIngressStreamRepository::mint(&mut streams, &SmSessionId::new("source-first"))
                .await
                .expect("first SM");
        let second_sm =
            SmIngressStreamRepository::mint(&mut streams, &SmSessionId::new("source-second"))
                .await
                .expect("second SM");
        streams.commit().await.expect("commit streams");
        let first_source = CanonicalSourceBinding::new(
            fixture.principal.clone(),
            Some((first_sm, IngressOrdinal::FIRST)),
        );
        let offer = || SourceAliasOffer {
            sender: &first.sender,
            target: &first.target,
            origin,
            digest: &digest,
            source: &first_source,
            envelope: &first_envelope,
        };
        let first_key = MessageKey::new();
        let (mut tx, result) = classify_source_alias(
            fixture.uow.begin().await.expect("first UOW"),
            offer(),
            || first_key,
        )
        .await
        .expect("classify first");
        assert_eq!(result, SourceAliasClassification::NewBound(first_key));
        SmIngressRepository::insert_sm_ref(
            &mut tx,
            first_sm,
            IngressOrdinal::FIRST,
            WireHandledCount::from_storage(1),
            first_key,
        )
        .await
        .expect("first SM ref");
        tx.commit().await.expect("commit first");

        let sibling = AuthenticatedPrincipalRef::new(
            fixture.principal.bare_jid().clone(),
            AuthContextId::new(Uuid::new_v4()),
            AuthContextVersion::new(7),
            PrincipalAuthEpoch::new(9),
        );
        add_session(&fixture, &sibling).await;
        let sibling_source =
            CanonicalSourceBinding::new(sibling, Some((second_sm, IngressOrdinal::FIRST)));
        let sibling_sender: jid::FullJid =
            "romeo@example.com/tablet".parse().expect("sibling sender");
        let mut changed_message = first.plan.sanitized_message.clone();
        changed_message.from = Some(sibling_sender.clone().into());
        let changed_envelope = MessageEnvelope::new(changed_message);
        let sibling_offer = || SourceAliasOffer {
            sender: &sibling_sender,
            target: &first.target,
            origin,
            digest: &digest,
            source: &sibling_source,
            envelope: &changed_envelope,
        };
        let (mut tx, replay) = classify_source_alias(
            fixture.uow.begin().await.expect("sibling UOW"),
            sibling_offer(),
            MessageKey::new,
        )
        .await
        .expect("sibling replay");
        assert_eq!(
            replay,
            SourceAliasClassification::ExistingBound {
                key: first_key,
                first_source: first_source.clone()
            }
        );
        SmIngressRepository::insert_sm_ref(
            &mut tx,
            second_sm,
            IngressOrdinal::FIRST,
            WireHandledCount::from_storage(1),
            first_key,
        )
        .await
        .expect("second SM ref");
        assert_eq!(
            CanonicalMessageRepository::load_envelope(&mut tx, first_key)
                .await
                .expect("load"),
            Some(first_envelope.clone())
        );
        tx.commit().await.expect("commit sibling");

        let other_digest =
            waddle_xmpp::ingress::SemanticDigest::from_storage(1, [8; 32]).expect("digest");
        let conflict = SourceAliasOffer {
            digest: &other_digest,
            ..sibling_offer()
        };
        let (tx, result) = classify_source_alias(
            fixture.uow.begin().await.expect("conflict UOW"),
            conflict,
            MessageKey::new,
        )
        .await
        .expect("conflict");
        assert_eq!(result, SourceAliasClassification::Conflict);
        tx.commit().await.expect("commit no-op conflict");
        let wrong_sender: jid::FullJid = "mallory@example.com/phone".parse().expect("wrong sender");
        let conflict = SourceAliasOffer {
            sender: &wrong_sender,
            ..sibling_offer()
        };
        let (tx, result) = classify_source_alias(
            fixture.uow.begin().await.expect("wrong sender UOW"),
            conflict,
            MessageKey::new,
        )
        .await
        .expect("sender conflict");
        assert_eq!(result, SourceAliasClassification::Conflict);
        tx.commit().await.expect("commit no-op sender conflict");

        let second = fixture.submission(Some("roll-back-alias"), "rollback body");
        let second_digest = waddle_xmpp::ingress::digest::v1::digest(&second.digest_input);
        let second_envelope = MessageEnvelope::new(second.plan.sanitized_message.clone());
        let source = CanonicalSourceBinding::new(fixture.principal.clone(), None);
        let second_offer = || SourceAliasOffer {
            sender: &second.sender,
            target: &second.target,
            origin: second.digest_input.origin().expect("origin"),
            digest: &second_digest,
            source: &source,
            envelope: &second_envelope,
        };
        let rolled_back_key = MessageKey::new();
        let (tx, result) = classify_source_alias(
            fixture.uow.begin().await.expect("rollback UOW"),
            second_offer(),
            || rolled_back_key,
        )
        .await
        .expect("insert before rollback");
        assert_eq!(result, SourceAliasClassification::NewBound(rolled_back_key));
        drop(tx);
        let replacement = MessageKey::new();
        let (tx, result) = classify_source_alias(
            fixture.uow.begin().await.expect("replacement UOW"),
            second_offer(),
            || replacement,
        )
        .await
        .expect("after rollback");
        assert_eq!(result, SourceAliasClassification::NewBound(replacement));
        tx.commit().await.expect("commit replacement");
        fixture.close().await;
    }

    #[tokio::test]
    async fn sqlite_legacy_empty_envelope_is_not_backfilled() {
        let fixture = IngressFixture::sqlite().await;
        let first = fixture.submission(Some("legacy-source-alias"), "legacy");
        let digest = waddle_xmpp::ingress::digest::v1::digest(&first.digest_input);
        let origin = first.digest_input.origin().expect("origin");
        let key = MessageKey::new();
        let mut tx = fixture.uow.begin().await.expect("legacy UOW");
        assert_eq!(
            CanonicalMessageRepository::resolve_and_record_alias(
                &mut tx,
                &first.sender.to_bare(),
                &first.target,
                origin,
                &digest,
                || key
            )
            .await
            .expect("legacy alias"),
            AliasResolution::Aliased(AliasOutcome::Inserted(key))
        );
        tx.commit().await.expect("commit legacy");
        let source = CanonicalSourceBinding::new(fixture.principal.clone(), None);
        let envelope = MessageEnvelope::new(first.plan.sanitized_message.clone());
        let offer = SourceAliasOffer {
            sender: &first.sender,
            target: &first.target,
            origin,
            digest: &digest,
            source: &source,
            envelope: &envelope,
        };
        let (mut tx, result) = classify_source_alias(
            fixture.uow.begin().await.expect("legacy replay UOW"),
            offer,
            MessageKey::new,
        )
        .await
        .expect("legacy replay");
        assert_eq!(result, SourceAliasClassification::LegacyUnbound(key));
        assert_eq!(
            CanonicalMessageRepository::load_envelope(&mut tx, key)
                .await
                .expect("legacy envelope"),
            None
        );
        tx.commit().await.expect("commit legacy replay");
        let source_version = fixture
            .optional_text("SELECT source_binding_version FROM ingress_messages LIMIT 1")
            .await;
        assert_eq!(source_version, None);
        fixture.close().await;
    }

    #[tokio::test]
    async fn sqlite_invalid_principal_drops_the_owned_uow() {
        let fixture = IngressFixture::sqlite().await;
        let first = fixture.submission(Some("missing-principal"), "body");
        let digest = waddle_xmpp::ingress::digest::v1::digest(&first.digest_input);
        let bad = AuthenticatedPrincipalRef::new(
            fixture.principal.bare_jid().clone(),
            AuthContextId::new(Uuid::new_v4()),
            AuthContextVersion::new(1),
            PrincipalAuthEpoch::new(1),
        );
        let source = CanonicalSourceBinding::new(bad, None);
        let envelope = MessageEnvelope::new(first.plan.sanitized_message.clone());
        let offer = SourceAliasOffer {
            sender: &first.sender,
            target: &first.target,
            origin: first.digest_input.origin().expect("origin"),
            digest: &digest,
            source: &source,
            envelope: &envelope,
        };
        assert!(matches!(
            classify_source_alias(
                fixture.uow.begin().await.expect("UOW"),
                offer,
                MessageKey::new
            )
            .await,
            Err(SourceAliasError::Uow(
                crate::ingress_uow::IngressUowError::PrincipalAssertionFailed
            ))
        ));
        assert_eq!(fixture.count("ingress_messages").await, 0);
        fixture.close().await;
    }

    #[tokio::test]
    async fn sqlite_temp_shadows_do_not_replace_main_alias_or_principal() {
        let fixture = IngressFixture::sqlite().await;
        let first = fixture.submission(Some("sqlite-shadow"), "body");
        let digest = waddle_xmpp::ingress::digest::v1::digest(&first.digest_input);
        let source = CanonicalSourceBinding::new(fixture.principal.clone(), None);
        let envelope = MessageEnvelope::new(first.plan.sanitized_message.clone());
        let mut tx = fixture.uow.begin().await.expect("UOW");
        for table in [
            "sessions",
            "ingress_messages",
            "ingress_origin_aliases",
            "ingress_protocol_epoch",
        ] {
            tx.transaction_mut()
                .execute(
                    &format!("CREATE TEMP TABLE {table} AS SELECT * FROM main.{table} WHERE 0"),
                    (),
                )
                .await
                .expect("shadow table");
        }
        let key = MessageKey::new();
        let offer = SourceAliasOffer {
            sender: &first.sender,
            target: &first.target,
            origin: first.digest_input.origin().expect("origin"),
            digest: &digest,
            source: &source,
            envelope: &envelope,
        };
        let (mut tx, result) = classify_source_alias(tx, offer, || key)
            .await
            .expect("main classification");
        assert_eq!(result, SourceAliasClassification::NewBound(key));
        let mut rows = tx.transaction_mut().query(
            "SELECT (SELECT COUNT(*) FROM main.ingress_messages), (SELECT COUNT(*) FROM temp.ingress_messages)", (),
        ).await.expect("count both");
        let row = rows.next().await.expect("count").expect("row");
        assert_eq!(row.get::<i64>(0).expect("main"), 1);
        assert_eq!(row.get::<i64>(1).expect("temp"), 0);
        drop(rows);
        tx.commit().await.expect("commit main");
        fixture.close().await;
    }
}
