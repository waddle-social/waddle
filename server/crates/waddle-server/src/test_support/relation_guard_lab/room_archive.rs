//! Exercise the production-compiled writer in the lab with the installed guard.
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use super::Lab;
use crate::config::LineageConfig;
use crate::db::physical_attestation::attested_pool;
use crate::ingress::recorded_room_archive::{
    RecordedRoomArchiveBounds, RecordedRoomArchiveError, RecordedRoomArchiveWriter,
};
use crate::ingress_substrate::{MessageEnvelope, MucArchiveContext};
use crate::ingress_uow::{
    CanonicalMessageRepository, EffectIntentRepository, IngressUowError, ReconcileVerdict,
};
use waddle_xmpp::ingress::{
    EffectMessageIdentity, EntityGeneration, IngressEffectIntent, MessageKey, SemanticDigest,
};
use waddle_xmpp::muc::RoomClaimFenceContext;
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity, SharedNodeIdentity};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::{Lang, Message, MessageType};

pub async fn verify_recorded_room_archive(admin_url: &str) -> anyhow::Result<()> {
    use futures::FutureExt;
    let lab = Lab::create_with_guard(admin_url, false).await?;
    let result = std::panic::AssertUnwindSafe(exercise(&lab))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("room archive fixture panicked")));
    let cleanup = lab.cleanup().await;
    match (result, cleanup) {
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("fixture cleanup failed: {cleanup}")))
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn bounds() -> RecordedRoomArchiveBounds {
    RecordedRoomArchiveBounds::new(
        Duration::from_secs(3),
        Duration::from_secs(5),
        Duration::from_secs(7),
    )
    .expect("bounds")
}

async fn make_writer(
    lab: &Lab,
    options: PgConnectOptions,
    identity: SharedNodeIdentity,
    bounds: RecordedRoomArchiveBounds,
) -> anyhow::Result<RecordedRoomArchiveWriter> {
    let pool = attested_pool(options, lab.config.clone(), 2, Duration::from_secs(5))?;
    Ok(RecordedRoomArchiveWriter::from_attested_pool(
        pool,
        LineageConfig {
            deployment_uuid: Some(lab.lineage.deployment_uuid),
            action: None,
        },
        identity,
        1,
        1,
        bounds,
    )
    .await?)
}

fn archive_source(
    room: &jid::BareJid,
    id: &StanzaId,
    body: &str,
    context: bool,
) -> MessageEnvelope {
    let mut source = Message::new(Some(room.clone().into()));
    source.type_ = MessageType::Groupchat;
    source.from = Some(format!("{room}/alice").parse().expect("sender"));
    source.id = Some(xmpp_parsers::message::Id("wire-id".to_owned()));
    source.bodies.insert(Lang::default(), body.to_owned());
    waddle_xmpp_core::xep0359::add_stanza_id(&mut source, id);
    waddle_xmpp::xep::xep0421::set_occupant_id_on_message(
        &mut source,
        &waddle_xmpp::xep::xep0421::OccupantId::new("occupant"),
    );
    let envelope = MessageEnvelope::new(source);
    if context {
        envelope.with_archive_context(MucArchiveContext {
            stanza_id: id.clone(),
            sender: Some(waddle_xmpp_core::mam::ArchivedMucSender {
                jid: "alice@example.test/original".parse().expect("real sender"),
                affiliation: waddle_xmpp::Affiliation::Member,
                role: waddle_xmpp::Role::Participant,
            }),
            nickname_generation: Some(9),
        })
    } else {
        envelope
    }
}

fn intents(room: &jid::BareJid, id: &StanzaId) -> Vec<IngressEffectIntent> {
    vec![
        IngressEffectIntent::ArchiveAuthoritative {
            archive: room.clone(),
            by: room.clone(),
            stanza_id: id.clone(),
            archived_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("time"),
            ordinal: None,
        },
        IngressEffectIntent::RouteMucGroupchat {
            room: room.clone(),
            occupants: vec![
                format!("{room}/alice").parse().expect("alice"),
                format!("{room}/bob").parse().expect("bob"),
            ],
            reflection: format!("{room}/alice").parse().expect("reflection"),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::StanzaId(id.clone()),
        },
    ]
}

async fn seed(
    lab: &Lab,
    key: MessageKey,
    envelope: &MessageEnvelope,
    intents: &[IngressEffectIntent],
) -> anyhow::Result<()> {
    let digest = SemanticDigest::from_storage(1, [1; 32])?;
    let mut tx = lab.lane.uow.begin().await.context("seed begin")?;
    CanonicalMessageRepository::record_message(&mut tx, key, &digest, Some(envelope))
        .await
        .context("seed canonical")?;
    let verdict = EffectIntentRepository::reconcile(&mut tx, key, intents, false)
        .await
        .context("seed intents")?;
    ensure!(
        !matches!(
            verdict,
            ReconcileVerdict::Contradiction { .. } | ReconcileVerdict::Divergent { .. }
        ),
        "failed to seed intents"
    );
    tx.commit().await?;
    Ok(())
}

async fn rows(lab: &Lab, key: MessageKey, id: &StanzaId) -> anyhow::Result<(i64, i64, i64)> {
    let archive = sqlx::query_scalar("SELECT count(*) FROM guarded.mam_messages WHERE id = $1")
        .bind(&id.id)
        .fetch_one(&lab.admin)
        .await?;
    let dispatch = sqlx::query_scalar(
        "SELECT count(*) FROM guarded.ingress_archive_dispatch WHERE message_key = $1",
    )
    .bind(key.to_storage())
    .fetch_one(&lab.admin)
    .await?;
    let receipt = sqlx::query_scalar(
        "SELECT count(*) FROM guarded.ingress_effect_receipts WHERE message_key = $1",
    )
    .bind(key.to_storage())
    .fetch_one(&lab.admin)
    .await?;
    Ok((archive, dispatch, receipt))
}

async fn ordinal(
    lab: &Lab,
    key: MessageKey,
) -> anyhow::Result<Option<waddle_xmpp::mam::ArchiveOrdinal>> {
    let mut tx = lab.lane.uow.begin().await?;
    let intents = EffectIntentRepository::load(&mut tx, key).await?;
    tx.commit().await?;
    Ok(intents.into_iter().find_map(|intent| match intent {
        IngressEffectIntent::ArchiveAuthoritative { ordinal, .. } => ordinal,
        _ => None,
    }))
}

async fn exercise(lab: &Lab) -> anyhow::Result<()> {
    let restricted: bool = sqlx::query_scalar(
        "SELECT NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole AND NOT rolbypassrls \
         AND NOT pg_catalog.has_schema_privilege(rolname, 'guarded', 'CREATE') \
         AND NOT pg_catalog.has_column_privilege(rolname, 'guarded.ingress_protocol_epoch', 'epoch', 'UPDATE') \
         FROM pg_catalog.pg_roles WHERE rolname = $1",
    )
    .bind(&lab.runtime)
    .fetch_one(&lab.admin)
    .await?;
    ensure!(restricted, "fixture writer role is not restricted");
    let guard: String = sqlx::query_scalar("SELECT pg_catalog.pg_get_functiondef('guarded.waddle_ingress_epoch_guard()'::regprocedure)")
        .fetch_one(&lab.admin).await?;
    ensure!(
        guard.contains("FOR SHARE"),
        "installed V1009 row guard was replaced"
    );
    sqlx::query("UPDATE guarded.ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = $1 WHERE id = 1")
        .bind(lab.lineage.lineage_uuid.0).execute(&lab.admin).await?;
    let mut unprepared = PgConnection::connect_with(&lab.runtime_options).await?;
    let failure =
        sqlx::query("UPDATE guarded.ingress_messages SET terminal_at = terminal_at WHERE false")
            .execute(&mut unprepared)
            .await
            .expect_err("unprepared write must fail");
    ensure!(
        failure
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref()
            == Some("P0001")
            && failure
                .to_string()
                .contains("requires a transaction-local epoch proof"),
        "V1009 did not reject the missing epoch proof"
    );
    unprepared.close().await?;
    let room: jid::BareJid = "archive@conference.example.test".parse()?;
    let owner = NodeIdentity::new("writer-node", "writer-incarnation");
    let identity = SharedNodeIdentity::new(owner.clone());
    let fence = RoomClaimFenceContext::new(
        Entity::new(EntityType::RoomActor, room.to_string()),
        owner.clone(),
        ClaimEpoch(7),
    );
    sqlx::query("INSERT INTO guarded.clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES ($1, $2, $3, $4, 7)")
        .bind(format!("room_actor:{room}")).bind("room_actor").bind(&owner.node_id).bind(&owner.node_epoch).execute(&lab.admin).await?;
    let options = lab
        .runtime_options
        .clone()
        .options([("search_path", "guarded,shadow")]);
    let writer = Arc::new(make_writer(lab, options, identity.clone(), bounds()).await?);
    let key = MessageKey::new();
    let id = StanzaId::new(Uuid::new_v4().to_string(), room.clone().into());
    seed(
        lab,
        key,
        &archive_source(&room, &id, "original", true),
        &intents(&room, &id),
    )
    .await?;
    let hostile = make_writer(
        lab,
        lab.runtime_options
            .clone()
            .options([("search_path", "shadow,guarded")]),
        identity.clone(),
        RecordedRoomArchiveBounds::new(
            Duration::from_millis(200),
            Duration::from_millis(250),
            Duration::from_millis(300),
        )?,
    )
    .await?;
    ensure!(
        matches!(
            hostile.write(key, &id, &fence).await,
            Err(RecordedRoomArchiveError::Timeout
                | RecordedRoomArchiveError::Preparation(IngressUowError::Timeout))
        ),
        "shadow-first checkout was not rejected"
    );
    ensure!(
        rows(lab, key, &id).await? == (0, 0, 0),
        "rejected checkout wrote data"
    );
    drop(hostile);
    let first = writer
        .write(key, &id, &fence)
        .await
        .context("initial room archive write")?;
    let stored: (String, Option<String>, Option<i64>, bool) = sqlx::query_as(
        "SELECT from_jid, body, nickname_generation, timestamp = pg_catalog.to_timestamp(1700000000) FROM guarded.mam_messages WHERE id = $1",
    ).bind(&id.id).fetch_one(&lab.admin).await?;
    ensure!(
        stored
            == (
                format!("{room}/alice"),
                Some("original".to_owned()),
                Some(9),
                true
            ),
        "projection did not preserve frozen sender, payload, nickname generation and time"
    );
    let rich: String =
        sqlx::query_scalar("SELECT rich_payload FROM guarded.mam_messages WHERE id = $1")
            .bind(&id.id)
            .fetch_one(&lab.admin)
            .await?;
    let rich: waddle_xmpp_core::mam::ArchivedRichMessage = serde_json::from_str(&rich)?;
    ensure!(
        rich.muc_sender.as_ref().is_some_and(|sender| sender.jid
            == "alice@example.test/original"
                .parse::<jid::Jid>()
                .expect("sender")),
        "archive did not preserve the frozen real sender"
    );
    ensure!(
        rows(lab, key, &id).await? == (1, 1, 1),
        "archive, dispatch and receipt were not committed together"
    );
    ensure!(
        ordinal(lab, key).await? == Some(first),
        "recorded ordinal missing"
    );
    ensure!(
        writer.write(key, &id, &fence).await? == first,
        "replay changed ordinal"
    );
    ensure!(
        rows(lab, key, &id).await? == (1, 1, 1),
        "replay duplicated durable rows"
    );
    sqlx::query("DELETE FROM guarded.mam_messages WHERE id = $1")
        .bind(&id.id)
        .execute(&lab.admin)
        .await?;
    ensure!(
        writer.write(key, &id, &fence).await? == first,
        "missing-row repair changed ordinal"
    );
    ensure!(
        rows(lab, key, &id).await? == (1, 1, 1) && ordinal(lab, key).await? == Some(first),
        "repair lost the original archive, dispatch or receipt identity"
    );
    let shadow: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM shadow.mam_messages) + (SELECT count(*) FROM shadow.ingress_archive_dispatch) + (SELECT count(*) FROM shadow.ingress_effect_receipts)").fetch_one(&lab.admin).await?;
    ensure!(shadow == 0, "hostile search path received writes");

    let collision = MessageKey::new();
    seed(
        lab,
        collision,
        &archive_source(&room, &id, "different", true),
        &intents(&room, &id),
    )
    .await?;
    ensure!(
        matches!(
            writer.write(collision, &id, &fence).await,
            Err(RecordedRoomArchiveError::Operation(
                IngressUowError::MamStore(
                    waddle_xmpp::mam::MamTxStoreError::ProjectionConflict { .. }
                )
            ))
        ),
        "conflicting archive payload was accepted"
    );
    ensure!(
        rows(lab, collision, &id).await? == (1, 0, 0) && ordinal(lab, collision).await?.is_none(),
        "conflicting archive changed authority"
    );

    let no_context = MessageKey::new();
    let missing_id = StanzaId::new(Uuid::new_v4().to_string(), room.clone().into());
    seed(
        lab,
        no_context,
        &archive_source(&room, &missing_id, "legacy", false),
        &intents(&room, &missing_id),
    )
    .await?;
    ensure!(
        matches!(
            writer.write(no_context, &missing_id, &fence).await,
            Err(RecordedRoomArchiveError::Ineligible)
        ),
        "legacy context was accepted"
    );
    ensure!(
        rows(lab, no_context, &missing_id).await? == (0, 0, 0),
        "legacy case wrote data"
    );

    for case in [
        "archive_by",
        "route_stamp",
        "missing_stamp",
        "system_archive",
        "mixed_system_archive",
    ] {
        let key = MessageKey::new();
        let id = StanzaId::new(Uuid::new_v4().to_string(), room.clone().into());
        let mut recorded = intents(&room, &id);
        let mut envelope = archive_source(&room, &id, "ineligible", true);
        match case {
            "archive_by" => {
                if let IngressEffectIntent::ArchiveAuthoritative { by, .. } = &mut recorded[0] {
                    *by = "other@conference.example.test".parse()?;
                }
            }
            "route_stamp" => {
                if let IngressEffectIntent::RouteMucGroupchat { route_identity, .. } =
                    &mut recorded[1]
                {
                    *route_identity = EffectMessageIdentity::StanzaId(StanzaId::new(
                        "different",
                        room.clone().into(),
                    ));
                }
            }
            "missing_stamp" => {
                let mut source = envelope.message().clone();
                waddle_xmpp_core::xep0359::remove_stanza_ids_by(&mut source, &id.by);
                envelope = MessageEnvelope::new(source).with_archive_context(MucArchiveContext {
                    stanza_id: id.clone(),
                    sender: None,
                    nickname_generation: Some(9),
                });
            }
            "mixed_system_archive" => {
                let other: jid::BareJid = "other@conference.example.test".parse()?;
                recorded.push(IngressEffectIntent::SystemMessageArchive {
                    sequence: 2,
                    archive: room.clone(),
                    by: other.clone(),
                    stanza_id: StanzaId::new(id.id.clone(), other.into()),
                    archived_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("time"),
                    ordinal: None,
                });
            }
            "system_archive" => {
                let IngressEffectIntent::ArchiveAuthoritative {
                    archive,
                    by,
                    stanza_id,
                    archived_at,
                    ordinal,
                } = recorded.remove(0)
                else {
                    unreachable!()
                };
                recorded.push(IngressEffectIntent::SystemMessageArchive {
                    sequence: 1,
                    archive,
                    by,
                    stanza_id,
                    archived_at,
                    ordinal,
                });
            }
            _ => unreachable!(),
        }
        seed(lab, key, &envelope, &recorded).await?;
        ensure!(
            matches!(
                writer.write(key, &id, &fence).await,
                Err(RecordedRoomArchiveError::Ineligible)
            ),
            "{case} was accepted"
        );
        ensure!(rows(lab, key, &id).await? == (0, 0, 0), "{case} wrote data");
    }

    let mut bad = fence.clone();
    bad.entity = Entity::new(EntityType::RoomActor, "other@conference.example.test");
    ensure!(
        matches!(
            writer.write(key, &id, &bad).await,
            Err(RecordedRoomArchiveError::Ineligible)
        ),
        "wrong room accepted"
    );
    bad = fence.clone();
    bad.epoch = ClaimEpoch(6);
    ensure!(
        matches!(
            writer.write(key, &id, &bad).await,
            Err(RecordedRoomArchiveError::Operation(
                IngressUowError::ClaimFenceMissing
            ))
        ),
        "stale epoch accepted"
    );
    bad = fence.clone();
    bad.owner = NodeIdentity::new("writer-node", "other-incarnation");
    ensure!(
        writer.write(key, &id, &bad).await.is_err(),
        "wrong incarnation accepted"
    );

    rollback_stages(lab, &writer, &room, &fence).await?;
    saturation_and_takeover(lab, &writer, &room, &fence).await?;
    lost_commit(lab, &room, &fence, identity.clone(), false).await?;
    lost_commit(lab, &room, &fence, identity, true).await?;
    Ok(())
}

async fn rollback_stages(
    lab: &Lab,
    writer: &RecordedRoomArchiveWriter,
    room: &jid::BareJid,
    fence: &RoomClaimFenceContext,
) -> anyhow::Result<()> {
    for (table, privileges) in [
        ("mam_archive_sequences", "INSERT, UPDATE"),
        ("mam_messages", "INSERT"),
        ("ingress_effect_intents", "UPDATE (payload)"),
        ("ingress_archive_dispatch", "INSERT"),
        ("ingress_effect_receipts", "INSERT"),
    ] {
        let key = MessageKey::new();
        let id = StanzaId::new(Uuid::new_v4().to_string(), room.clone().into());
        seed(
            lab,
            key,
            &archive_source(room, &id, "rollback", true),
            &intents(room, &id),
        )
        .await?;
        let before: i64 = sqlx::query_scalar(
            "SELECT next_seq FROM guarded.mam_archive_sequences WHERE archive_jid = $1",
        )
        .bind(room.to_string())
        .fetch_one(&lab.admin)
        .await?;
        sqlx::query(&format!(
            "REVOKE {privileges} ON guarded.{table} FROM {}",
            lab.runtime
        ))
        .execute(&lab.admin)
        .await?;
        let result = writer.write(key, &id, fence).await;
        sqlx::query(&format!(
            "GRANT {privileges} ON guarded.{table} TO {}",
            lab.runtime
        ))
        .execute(&lab.admin)
        .await?;
        ensure!(result.is_err(), "revoking {table} did not fail the write");
        ensure!(
            rows(lab, key, &id).await? == (0, 0, 0),
            "failure at {table} left a partial archive"
        );
        ensure!(
            ordinal(lab, key).await?.is_none(),
            "failure at {table} left a finalized ordinal"
        );
        let after: i64 = sqlx::query_scalar(
            "SELECT next_seq FROM guarded.mam_archive_sequences WHERE archive_jid = $1",
        )
        .bind(room.to_string())
        .fetch_one(&lab.admin)
        .await?;
        ensure!(
            before == after,
            "failure at {table} advanced archive sequence"
        );
    }
    Ok(())
}

async fn saturation_and_takeover(
    lab: &Lab,
    writer: &Arc<RecordedRoomArchiveWriter>,
    room: &jid::BareJid,
    fence: &RoomClaimFenceContext,
) -> anyhow::Result<()> {
    let key1 = MessageKey::new();
    let key2 = MessageKey::new();
    let id1 = StanzaId::new(Uuid::new_v4().to_string(), room.clone().into());
    let id2 = StanzaId::new(Uuid::new_v4().to_string(), room.clone().into());
    seed(
        lab,
        key1,
        &archive_source(room, &id1, "saturation one", true),
        &intents(room, &id1),
    )
    .await?;
    seed(
        lab,
        key2,
        &archive_source(room, &id2, "saturation two", true),
        &intents(room, &id2),
    )
    .await?;
    let mut blocker = lab.admin.acquire().await?;
    sqlx::query("BEGIN").execute(&mut *blocker).await?;
    sqlx::query("SELECT 1 FROM guarded.ingress_messages WHERE message_key = $1 FOR UPDATE")
        .bind(key1.to_storage())
        .execute(&mut *blocker)
        .await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    let first = {
        let writer = writer.clone();
        let id = id1.clone();
        let fence = fence.clone();
        tokio::spawn(async move { writer.write(key1, &id, &fence).await })
    };
    super::wait_for_blocked(lab, &lab.application, pid).await?;
    let second = {
        let writer = writer.clone();
        let id = id2.clone();
        let fence = fence.clone();
        tokio::spawn(async move { writer.write(key2, &id, &fence).await })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE application_name = $1 AND state = 'active'").bind(&lab.application).fetch_one(&lab.admin).await?;
    ensure!(
        active == 1,
        "one-slot ingress lane checked out more than one writer connection: {active}"
    );
    sqlx::query("COMMIT").execute(&mut *blocker).await?;
    first.await?.context("first saturated write")?;
    second.await?.context("second saturated write")?;
    ensure!(
        rows(lab, key1, &id1).await? == (1, 1, 1) && rows(lab, key2, &id2).await? == (1, 1, 1),
        "saturated writer did not complete both operations"
    );

    sqlx::query("BEGIN").execute(&mut *blocker).await?;
    sqlx::query("UPDATE guarded.clustering_claims SET claim_epoch = 8 WHERE entity = $1")
        .bind(format!("room_actor:{room}"))
        .execute(&mut *blocker)
        .await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    let stale = {
        let writer = writer.clone();
        let id = id1.clone();
        let fence = fence.clone();
        tokio::spawn(async move { writer.write(key1, &id, &fence).await })
    };
    super::wait_for_blocked(lab, &lab.application, pid).await?;
    sqlx::query("COMMIT").execute(&mut *blocker).await?;
    ensure!(
        matches!(
            stale.await?,
            Err(RecordedRoomArchiveError::Operation(
                IngressUowError::ClaimFenceMissing
            ))
        ),
        "takeover did not fence the queued writer"
    );
    sqlx::query("UPDATE guarded.clustering_claims SET claim_epoch = 7 WHERE entity = $1")
        .bind(format!("room_actor:{room}"))
        .execute(&lab.admin)
        .await?;
    Ok(())
}

async fn lost_commit(
    lab: &Lab,
    room: &jid::BareJid,
    fence: &RoomClaimFenceContext,
    identity: SharedNodeIdentity,
    hold: bool,
) -> anyhow::Result<()> {
    let key = MessageKey::new();
    let id = StanzaId::new(Uuid::new_v4().to_string(), room.clone().into());
    seed(
        lab,
        key,
        &archive_source(room, &id, "ambiguous commit", true),
        &intents(room, &id),
    )
    .await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let held = Arc::new(tokio::sync::Notify::new());
    let disconnect = Arc::new(tokio::sync::Notify::new());
    let proxy = tokio::spawn(super::lose_commit_reply(
        listener,
        lab.runtime_options.clone(),
        held.clone(),
        disconnect.clone(),
        b"ingress_effect_receipts",
    ));
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .database(&lab.database_name)
        .username(&lab.runtime)
        .password(&lab.password)
        .application_name(&lab.application)
        .ssl_mode(sqlx::postgres::PgSslMode::Disable)
        .options([("search_path", "guarded,shadow")]);
    let deadline = if hold {
        Duration::from_secs(4)
    } else {
        Duration::from_secs(7)
    };
    let writer = make_writer(
        lab,
        options,
        identity,
        RecordedRoomArchiveBounds::new(Duration::from_secs(3), Duration::from_secs(5), deadline)?,
    )
    .await?;
    let write = writer.write(key, &id, fence);
    let observe = async {
        tokio::time::timeout(Duration::from_secs(8), held.notified()).await?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if rows(lab, key, &id).await? == (1, 1, 1) {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        if !hold {
            disconnect.notify_one();
        }
        Ok::<(), anyhow::Error>(())
    };
    let (result, observed) = tokio::join!(write, observe);
    observed?;
    ensure!(
        matches!(result, Err(RecordedRoomArchiveError::CommitIndeterminate)),
        "lost or held acknowledgement was not indeterminate"
    );
    disconnect.notify_one();
    proxy.await??;
    ensure!(
        rows(lab, key, &id).await? == (1, 1, 1) && ordinal(lab, key).await?.is_some(),
        "durable chain missing after lost acknowledgement"
    );
    Ok(())
}
