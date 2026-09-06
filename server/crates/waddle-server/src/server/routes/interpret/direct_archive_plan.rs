//! Read-only call-state overlay shared by the sender and recipient archive passes.
use super::effects::direct::{PlannedActiveDmCall, PlannedDmCallState};
use super::*;
use crate::server::routes::websocket::{ActiveCallThread, DmCallThreadKey, PendingDmCallOffer};

fn snapshot(deps: &Deps<'_>, key: DmCallThreadKey) -> Option<PlannedDmCallState> {
    for effect in deps.effects.snapshot().into_iter().rev() {
        if let super::effects::Effect::External(super::effects::ExternalEffect::Direct(
            ExternalDirectEffect::DmCallThreadState { state },
        )) = effect.effect
        {
            if state.key == key {
                return Some(*state);
            }
        }
    }
    let state = deps.web_socket_state?;
    let now = chrono::Utc::now();
    let pending = state
        .deps
        .protocol
        .pending_dm_call_offers
        .get(&key)
        .filter(|entry| {
            now.signed_duration_since(entry.started).num_seconds() <= DM_CALL_PENDING_TTL_SECS
        })
        .map(|entry| entry.clone());
    let active = state
        .deps
        .protocol
        .dm_call_threads
        .get(&key)
        .filter(|entry| {
            now.signed_duration_since(entry.started).num_seconds() <= DM_CALL_ACTIVE_TTL_SECS
        })
        .and_then(|entry| {
            Some(PlannedActiveDmCall {
                anchor: (!entry.anchor_origin_id.is_empty()).then(|| {
                    StanzaId::new(
                        entry.anchor_origin_id.clone(),
                        jid::Jid::from(if entry.initiator == key.low_peer {
                            key.high_peer.clone()
                        } else {
                            key.low_peer.clone()
                        }),
                    )
                }),
                initiator: entry.initiator.clone(),
                media: entry.media,
                started: entry.started,
                thread: waddle_xmpp_core::mam::ThreadId::new(entry.thread_id.clone())?,
            })
        });
    let projected = if active.is_some() {
        [&key.low_peer, &key.high_peer]
            .into_iter()
            .filter(|owner| {
                state
                    .deps
                    .protocol
                    .dm_call_thread_projections
                    .contains(&((*owner).clone(), key.clone()))
            })
            .cloned()
            .collect()
    } else {
        Default::default()
    };
    Some(PlannedDmCallState {
        key,
        pending,
        active,
        projected,
    })
}

fn record(deps: &Deps<'_>, state: PlannedDmCallState) {
    external(
        deps,
        ExternalDirectEffect::DmCallThreadState {
            state: Box::new(state),
        },
    );
}

pub(super) fn prepare(
    deps: &Deps<'_>,
    archive: &BareJid,
    from: &BareJid,
    to: &BareJid,
    message: &Message,
) -> Option<ActiveCallThread> {
    if !waddle_xmpp::xep::HintCarrier::has_store(message) {
        return None;
    }
    let key = DmCallThreadKey::new(from.clone(), to.clone(), jmi_sid(message, "proceed")?);
    let mut state = snapshot(deps, key)?;
    if state.projected.contains(archive) {
        return None;
    }
    if state.active.is_none() {
        let offer = state.pending.as_ref()?;
        if offer.initiator == *from {
            return None;
        }
        state.active = Some(PlannedActiveDmCall {
            anchor: None,
            initiator: offer.initiator.clone(),
            media: offer.media,
            started: chrono::Utc::now(),
            thread: waddle_xmpp_core::mam::ThreadId::new(state.key.sid.0.clone())?,
        });
        record(deps, state.clone());
    }
    let active = state.active?;
    if active.initiator == *from {
        return None;
    }
    Some(ActiveCallThread {
        anchor_origin_id: active.anchor.map(|id| id.id).unwrap_or_default(),
        initiator: active.initiator,
        media: active.media,
        started: active.started,
        thread_id: active.thread.as_str().to_owned(),
    })
}

pub(super) async fn project(
    deps: &Deps<'_>,
    archive: &BareJid,
    from: &BareJid,
    to: &BareJid,
    archive_id: &StanzaId,
    message: &Message,
) {
    if let Some((sid, media)) = jmi_propose(message) {
        let Some(mut state) = snapshot(deps, DmCallThreadKey::new(from.clone(), to.clone(), sid))
        else {
            return;
        };
        state.pending = Some(PendingDmCallOffer {
            media,
            initiator: from.clone(),
            started: chrono::Utc::now(),
        });
        record(deps, state);
        return;
    }
    if let Some(sid) = jmi_sid(message, "finish") {
        let Some(mut state) = snapshot(deps, DmCallThreadKey::new(from.clone(), to.clone(), sid))
        else {
            return;
        };
        state.pending = None;
        let active = state.active.take();
        state.projected.clear();
        record(deps, state.clone());
        if let Some(active) = active {
            let ended = chrono::Utc::now();
            let duration = waddle_xmpp::xep::CallThreadDuration::parse(
                &format_call_thread_duration(ended.signed_duration_since(active.started)),
            )
            .expect("valid duration");
            super::super::direct_call_thread::mark_direct_call_thread_ended(
                deps,
                state.key.low_peer,
                state.key.high_peer,
                active.thread.as_str().to_owned(),
                ended,
                duration,
            )
            .await;
        }
        return;
    }
    let Some(sid) = jmi_sid(message, "proceed") else {
        return;
    };
    let Some(mut state) = snapshot(deps, DmCallThreadKey::new(from.clone(), to.clone(), sid))
    else {
        return;
    };
    let Some(active) = state.active.as_mut() else {
        return;
    };
    if active.initiator == *from || state.projected.contains(archive) {
        return;
    }
    active.anchor.get_or_insert_with(|| archive_id.clone());
    let (thread, media) = (active.thread.clone(), active.media);
    state.projected.insert(archive.clone());
    if state.projected.contains(&state.key.low_peer)
        && state.projected.contains(&state.key.high_peer)
    {
        state.pending = None;
    }
    record(deps, state);
    let peer = if archive == from { to } else { from };
    super::super::direct_call_thread::project_direct_call_thread_anchor(
        deps,
        archive.clone(),
        peer.clone(),
        thread.as_str().to_owned(),
        archive_id.id.clone(),
        media,
        crate::time::now_ms(),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::super::effects::{direct::DurableDirectEffect, DurableEffect, Effect, PlanSink};
    use super::*;
    use waddle_xmpp::{
        inbox::storage::{InMemoryInboxStorage, InboxStorage},
        mam::{storage::InMemoryMamStorage, MamStorage},
        registry::ConnectionRegistry,
    };

    #[tokio::test]
    async fn finish_overlay_projects_each_peer_once_across_both_archives() {
        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        let sink = PlanSink::new();
        let mut deps = Deps::test_with_storage(&registry, &mam, &inbox);
        deps.effects = &sink;
        let alice: BareJid = "alice@example.com".parse().expect("alice");
        let bob: BareJid = "bob@example.com".parse().expect("bob");
        record(
            &deps,
            PlannedDmCallState {
                key: DmCallThreadKey::new(
                    alice.clone(),
                    bob.clone(),
                    xmpp_parsers::jingle::SessionId("call-1".into()),
                ),
                pending: None,
                active: Some(PlannedActiveDmCall {
                    anchor: None,
                    initiator: alice.clone(),
                    media: waddle_xmpp::xep::CallThreadMedia::audio_video(),
                    started: chrono::Utc::now(),
                    thread: waddle_xmpp_core::mam::ThreadId::new("call-1").expect("thread"),
                }),
                projected: [alice.clone(), bob.clone()].into_iter().collect(),
            },
        );
        let mut message = Message::new(Some(jid::Jid::from(bob.clone())));
        message.payloads.push(
            Element::builder("finish", waddle_xmpp::xep::xep0353::NS_JINGLE_MESSAGE)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "call-1")
                .build(),
        );
        for archive in [&alice, &bob] {
            let id = StanzaId::new("finish-id", jid::Jid::from(archive.clone()));
            project(&deps, archive, &alice, &bob, &id, &message).await;
        }
        assert_eq!(
            sink.snapshot()
                .iter()
                .filter(|planned| matches!(
                    planned.effect,
                    Effect::Durable(DurableEffect::Direct(
                        DurableDirectEffect::DmCallThreadProjection { .. }
                    ))
                ))
                .count(),
            2,
            "each peer is updated exactly once"
        );
        assert!(inbox
            .list_threads(&alice, &bob)
            .await
            .expect("read")
            .is_empty());
    }
}
