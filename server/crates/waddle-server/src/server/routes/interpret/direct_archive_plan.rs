//! Read-only call-state overlay shared by the sender and recipient archive passes.
use super::effects::direct::{PlannedActiveDmCall, PlannedDmCallState};
use super::*;
use crate::server::routes::websocket::{ActiveCallThread, DmCallThreadKey, PendingDmCallOffer};

fn snapshot(deps: &Deps<'_>, key: DmCallThreadKey) -> Option<PlannedDmCallState> {
    for effect in deps.effects.snapshot().into_iter().rev() {
        if let super::effects::Effect::External(super::effects::ExternalEffect::Direct(
            ExternalDirectEffect::DmCallThreadState { state, .. },
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
    let sequence = deps
        .effects
        .snapshot()
        .iter()
        .filter(|effect| {
            matches!(
                effect.effect,
                super::effects::Effect::External(super::effects::ExternalEffect::Direct(
                    ExternalDirectEffect::DmCallThreadState { .. }
                ))
            )
        })
        .count() as u64;
    let receipt = waddle_xmpp::ingress::IngressEffectIntent::DmCallThreadState {
        sequence,
        state: Box::new(state.clone()),
    };
    deps.capture_intent(receipt.clone());
    external(
        deps,
        ExternalDirectEffect::DmCallThreadState {
            state: Box::new(state),
            receipt: Some(Box::new(receipt)),
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
    async fn stored_jmi_propose_captures_frozen_call_state_before_execution() {
        let socket = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let protocol = &socket.deps.protocol;
        let sink = PlanSink::new();
        let capture = crate::ingress::IngressEffectCapture::new();
        let mut deps = Deps::test_with_storage(
            &protocol.connection_registry,
            &protocol.mam_storage,
            &protocol.inbox_storage,
        );
        deps.web_socket_state = Some(&socket);
        deps.effects = &sink;
        deps.ingress_effect_capture = Some(capture.clone());
        let alice: BareJid = "alice@example.com".parse().expect("alice");
        let bob: BareJid = "bob@example.com".parse().expect("bob");
        let mut message = Message::new(Some(bob.clone().into()));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        waddle_xmpp::xep::xep0334::add_hint(&mut message, waddle_xmpp::xep::xep0334::Hint::Store);
        message.payloads.push(
            Element::builder("propose", waddle_xmpp::xep::xep0353::NS_JINGLE_MESSAGE)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "call-offer")
                .append(
                    Element::builder("description", waddle_xmpp::xep::xep0167::NS_JINGLE_RTP)
                        .attr(minidom::rxml::xml_ncname!("media").to_owned(), "audio")
                        .build(),
                )
                .build(),
        );
        project(
            &deps,
            &alice,
            &alice,
            &bob,
            &StanzaId::new("stored", alice.clone().into()),
            &message,
        )
        .await;
        let intents = capture.snapshot().intents;
        let [waddle_xmpp::ingress::IngressEffectIntent::DmCallThreadState { sequence: 0, state }] =
            intents.as_slice()
        else {
            panic!("frozen offer intent")
        };
        assert_eq!(
            state.pending.as_ref().expect("pending offer").initiator,
            alice
        );
        assert!(
            protocol.pending_dm_call_offers.is_empty(),
            "Phase A did not mutate call state"
        );
        assert!(matches!(&sink.snapshot()[0].effect,
            Effect::External(super::super::effects::ExternalEffect::Direct(ExternalDirectEffect::DmCallThreadState { receipt: Some(receipt), .. })) if receipt.as_ref() == &intents[0]));
    }

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

    fn call_state(index: usize, started: chrono::DateTime<chrono::Utc>) -> PlannedDmCallState {
        let alice: BareJid = "alice@example.com".parse().expect("alice");
        let bob: BareJid = "bob@example.com".parse().expect("bob");
        let sid = format!("call-{index}");
        PlannedDmCallState {
            key: DmCallThreadKey::new(
                alice.clone(),
                bob.clone(),
                xmpp_parsers::jingle::SessionId(sid.clone()),
            ),
            pending: Some(PendingDmCallOffer {
                media: waddle_xmpp::xep::CallThreadMedia::audio_only(),
                initiator: alice.clone(),
                started,
            }),
            active: Some(PlannedActiveDmCall {
                anchor: None,
                initiator: alice.clone(),
                media: waddle_xmpp::xep::CallThreadMedia::audio_only(),
                started,
                thread: waddle_xmpp_core::mam::ThreadId::new(sid).expect("thread"),
            }),
            projected: [alice, bob].into_iter().collect(),
        }
    }

    async fn apply_call_plan(deps: &Deps<'_>, sink: &PlanSink, state: PlannedDmCallState) {
        use super::super::effects::{EffectOutcome, EffectSink, ImmediateSink};

        record(deps, state);
        for effect in sink.take().0 {
            assert!(matches!(
                ImmediateSink.execute(effect, deps).await,
                EffectOutcome::ConfirmedIntents(_)
            ));
        }
    }

    #[tokio::test]
    async fn planned_call_state_prunes_expired_offers_threads_and_orphan_projections() {
        let socket = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let protocol = &socket.deps.protocol;
        let sink = PlanSink::new();
        let mut deps = Deps::test_with_storage(
            &protocol.connection_registry,
            &protocol.mam_storage,
            &protocol.inbox_storage,
        );
        deps.web_socket_state = Some(&socket);
        deps.effects = &sink;
        let now = chrono::Utc::now();
        let active_expired = call_state(
            0,
            now - chrono::Duration::seconds(DM_CALL_ACTIVE_TTL_SECS + 2),
        );
        let active_key = active_expired.key.clone();
        apply_call_plan(&deps, &sink, active_expired).await;
        let pending_expired = call_state(
            1,
            now - chrono::Duration::seconds(DM_CALL_PENDING_TTL_SECS + 2),
        );
        let pending_key = pending_expired.key.clone();
        apply_call_plan(&deps, &sink, pending_expired).await;
        let mut orphan = call_state(2, now);
        orphan.active = None;
        let orphan_key = orphan.key.clone();
        apply_call_plan(&deps, &sink, orphan).await;
        let fresh = call_state(3, now);
        let fresh_key = fresh.key.clone();
        apply_call_plan(&deps, &sink, fresh).await;

        assert!(!protocol.pending_dm_call_offers.contains_key(&active_key));
        assert!(!protocol.dm_call_threads.contains_key(&active_key));
        assert!(!protocol.pending_dm_call_offers.contains_key(&pending_key));
        assert!(protocol.dm_call_threads.contains_key(&pending_key));
        assert!(protocol.pending_dm_call_offers.contains_key(&fresh_key));
        assert!(protocol.dm_call_threads.contains_key(&fresh_key));
        for key in [&active_key, &orphan_key] {
            for owner in [&key.low_peer, &key.high_peer] {
                assert!(!protocol
                    .dm_call_thread_projections
                    .contains(&(owner.clone(), key.clone())));
            }
        }
        assert_eq!(protocol.dm_call_thread_projections.len(), 4);
    }

    #[tokio::test]
    async fn planned_call_state_prunes_oldest_keys_at_the_immediate_path_cap() {
        let socket = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let protocol = &socket.deps.protocol;
        let sink = PlanSink::new();
        let mut deps = Deps::test_with_storage(
            &protocol.connection_registry,
            &protocol.mam_storage,
            &protocol.inbox_storage,
        );
        deps.web_socket_state = Some(&socket);
        deps.effects = &sink;
        let now = chrono::Utc::now();
        for index in 0..DM_CALL_STATE_MAX_KEYS + 2 {
            let started =
                now - chrono::Duration::milliseconds((DM_CALL_STATE_MAX_KEYS + 2 - index) as i64);
            apply_call_plan(&deps, &sink, call_state(index, started)).await;
        }
        // The immediate path prunes before insertion: the new key may take the
        // maps one above the cap until the following committed mutation.
        assert_eq!(
            protocol.pending_dm_call_offers.len(),
            DM_CALL_STATE_MAX_KEYS + 1
        );
        assert_eq!(protocol.dm_call_threads.len(), DM_CALL_STATE_MAX_KEYS + 1);
        apply_call_plan(&deps, &sink, call_state(DM_CALL_STATE_MAX_KEYS + 1, now)).await;
        assert_eq!(
            protocol.pending_dm_call_offers.len(),
            DM_CALL_STATE_MAX_KEYS
        );
        assert_eq!(protocol.dm_call_threads.len(), DM_CALL_STATE_MAX_KEYS);
        assert_eq!(
            protocol.dm_call_thread_projections.len(),
            DM_CALL_STATE_MAX_KEYS * 2
        );
        for index in 0..2 {
            let key = call_state(index, now).key;
            assert!(!protocol.pending_dm_call_offers.contains_key(&key));
            assert!(!protocol.dm_call_threads.contains_key(&key));
            for owner in [&key.low_peer, &key.high_peer] {
                assert!(!protocol
                    .dm_call_thread_projections
                    .contains(&(owner.clone(), key.clone())));
            }
        }
        assert!(protocol
            .dm_call_threads
            .contains_key(&call_state(2, now).key));
    }
}
