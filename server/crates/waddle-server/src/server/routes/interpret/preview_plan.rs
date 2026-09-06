//! Freeze preview-reference writes from read-only database and plan snapshots.
use std::collections::BTreeMap;

use jid::BareJid;
use tracing::warn;
use uuid::Uuid;
use waddle_xmpp::{
    ingress::{IngressEffectIntent, LinkPreviewMediaRefMutation, LinkPreviewMediaRefState},
    mam::RichMessageId,
};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::Message;

use super::{
    effects::{
        direct::{external, ExternalDirectEffect},
        Effect, ExternalEffect,
    },
    Deps,
};
use crate::db::{actor::DbQuery, row_value, Value, ValueExt};

pub(super) async fn update(
    deps: &Deps<'_>,
    archive: &BareJid,
    message_id: &RichMessageId,
    archive_id: &StanzaId,
    message: &Message,
) {
    let Some(state) = deps.web_socket_state else {
        return;
    };
    let mut mutations = current_references(deps, archive, message_id).await;
    for slot in crate::server::routes::websocket::link_preview_refs::cached_preview_upload_slot_ids(
        message,
        state.deps.auth_state.base_url.as_str(),
    ) {
        let Ok(upload_slot_id) = Uuid::parse_str(&slot) else {
            continue;
        };
        mutations.push(LinkPreviewMediaRefMutation {
            upload_slot_id,
            archive: archive.clone(),
            message_id: message_id.clone(),
            current_archive_stanza_id: archive_id.clone(),
            state: LinkPreviewMediaRefState::Current,
        });
    }
    record(deps, mutations, false);
}

pub(super) async fn clear(deps: &Deps<'_>, archive: &BareJid, message_id: &RichMessageId) {
    record(
        deps,
        current_references(deps, archive, message_id).await,
        true,
    );
}

fn record(deps: &Deps<'_>, mutations: Vec<LinkPreviewMediaRefMutation>, clearing: bool) {
    if mutations.is_empty() {
        return;
    }
    for mutation in &mutations {
        deps.capture_intent(IngressEffectIntent::LinkPreviewMediaRef {
            mutation: mutation.clone(),
        });
    }
    let effect = if clearing {
        ExternalDirectEffect::ClearLinkPreviewRefs { mutations }
    } else {
        ExternalDirectEffect::LinkPreviewRefs { mutations }
    };
    external(deps, effect);
}

async fn current_references(
    deps: &Deps<'_>,
    archive: &BareJid,
    message_id: &RichMessageId,
) -> Vec<LinkPreviewMediaRefMutation> {
    let mut refs = stored_references(deps, archive, message_id).await;
    // A correction/retraction later in the same plan observes preceding planned refs.
    for planned in deps.effects.snapshot() {
        let Effect::External(ExternalEffect::Direct(effect)) = planned.effect else {
            continue;
        };
        let (ExternalDirectEffect::LinkPreviewRefs { mutations }
        | ExternalDirectEffect::ClearLinkPreviewRefs { mutations }) = effect
        else {
            continue;
        };
        for mutation in mutations {
            if mutation.archive != *archive || mutation.message_id != *message_id {
                continue;
            }
            match mutation.state {
                LinkPreviewMediaRefState::Current => {
                    refs.insert(mutation.upload_slot_id, mutation.current_archive_stanza_id);
                }
                LinkPreviewMediaRefState::Unreferenced => {
                    refs.remove(&mutation.upload_slot_id);
                }
            }
        }
    }
    refs.into_iter()
        .map(
            |(upload_slot_id, current_archive_stanza_id)| LinkPreviewMediaRefMutation {
                upload_slot_id,
                archive: archive.clone(),
                message_id: message_id.clone(),
                current_archive_stanza_id,
                state: LinkPreviewMediaRefState::Unreferenced,
            },
        )
        .collect()
}

async fn stored_references(
    deps: &Deps<'_>,
    archive: &BareJid,
    message_id: &RichMessageId,
) -> BTreeMap<Uuid, StanzaId> {
    let Some(state) = deps.web_socket_state else {
        return BTreeMap::new();
    };
    let rows = match state.deps.app_state.db_pool.global_actor().ask(DbQuery {
        sql: "SELECT upload_slot_id, current_archive_id FROM link_preview_media_refs WHERE archive_jid = ? AND message_id = ? AND state = ?".to_owned(),
        params: vec![Value::from(archive.to_string()), Value::from(message_id.as_str().to_owned()), Value::from("current".to_owned())],
    }).await {
        Ok(rows) => rows,
        Err(error) => { warn!(%error, %archive, "could not read preview references during planning"); return BTreeMap::new(); }
    };
    rows.into_iter()
        .filter_map(|row| {
            let slot = row_value(&row, 0).and_then(ValueExt::as_string).ok()?;
            let upload_slot_id = Uuid::parse_str(&slot).ok()?;
            let archive_id = row_value(&row, 1).and_then(ValueExt::as_string).ok()?;
            Some((
                upload_slot_id,
                StanzaId::new(archive_id, jid::Jid::from(archive.clone())),
            ))
        })
        .collect()
}

/// Execute the frozen mutations without re-reading mutable reference state.
pub(super) async fn execute(
    deps: &Deps<'_>,
    mutations: Vec<LinkPreviewMediaRefMutation>,
) -> super::effects::EffectOutcome {
    use crate::db::actor::DbExecute;
    let Some(state) = deps.web_socket_state else {
        return super::effects::EffectOutcome::Unavailable;
    };
    for mutation in mutations {
        let now = chrono::Utc::now().to_rfc3339();
        let query = match mutation.state {
            LinkPreviewMediaRefState::Current => DbExecute {
                sql: "INSERT INTO link_preview_media_refs (upload_slot_id, archive_jid, message_id, current_archive_id, state, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (upload_slot_id, archive_jid, message_id) DO UPDATE SET current_archive_id = excluded.current_archive_id, state = excluded.state, updated_at = excluded.updated_at".to_owned(),
                params: vec![Value::from(mutation.upload_slot_id.to_string()), Value::from(mutation.archive.to_string()), Value::from(mutation.message_id.as_str().to_owned()), Value::from(mutation.current_archive_stanza_id.id), Value::from("current".to_owned()), Value::from(now.clone()), Value::from(now)],
            },
            LinkPreviewMediaRefState::Unreferenced => DbExecute {
                sql: "UPDATE link_preview_media_refs SET state = ?, updated_at = ? WHERE upload_slot_id = ? AND archive_jid = ? AND message_id = ? AND current_archive_id = ? AND state = ?".to_owned(),
                params: vec![Value::from("unreferenced".to_owned()), Value::from(now), Value::from(mutation.upload_slot_id.to_string()), Value::from(mutation.archive.to_string()), Value::from(mutation.message_id.as_str().to_owned()), Value::from(mutation.current_archive_stanza_id.id), Value::from("current".to_owned())],
            },
        };
        if let Err(error) = state.deps.app_state.db_pool.global_actor().ask(query).await {
            warn!(%error, "planned preview reference write failed");
            return super::effects::EffectOutcome::Unavailable;
        }
    }
    super::effects::EffectOutcome::Completed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress::IngressEffectCapture;
    use crate::server::routes::interpret::effects::PlanSink;
    use waddle_xmpp::registry::ConnectionRegistry;

    #[tokio::test]
    async fn clear_freezes_prior_planned_references_and_captures_the_mutations() {
        let registry = ConnectionRegistry::new();
        let sink = PlanSink::new();
        let capture = IngressEffectCapture::new();
        let mut deps =
            Deps::registry_only(&registry).with_ingress_effect_capture(Some(capture.clone()));
        deps.effects = &sink;
        let archive: BareJid = "alice@example.com".parse().expect("archive");
        let message_id = RichMessageId::new("wire-1".to_owned()).expect("message id");
        let current = LinkPreviewMediaRefMutation {
            upload_slot_id: Uuid::nil(),
            archive: archive.clone(),
            message_id: message_id.clone(),
            current_archive_stanza_id: StanzaId::new("archive-1", jid::Jid::from(archive.clone())),
            state: LinkPreviewMediaRefState::Current,
        };
        record(&deps, vec![current.clone()], false);
        clear(&deps, &archive, &message_id).await;
        let mut expected = current;
        expected.state = LinkPreviewMediaRefState::Unreferenced;
        let plan = sink.snapshot();
        assert!(matches!(&plan[1].effect,
            Effect::External(ExternalEffect::Direct(ExternalDirectEffect::ClearLinkPreviewRefs { mutations })) if mutations == &vec![expected.clone()]));
        assert!(capture
            .snapshot()
            .intents
            .contains(&IngressEffectIntent::LinkPreviewMediaRef { mutation: expected }));
        clear(&deps, &archive, &message_id).await;
        assert_eq!(
            sink.snapshot().len(),
            2,
            "a second clear sees the virtual unreferenced state"
        );
    }
}
