use chrono;
use jid::{BareJid, FullJid, Jid};
use kameo::actor::ActorRef;
use std::{collections::HashSet, sync::Arc};
use tracing::{debug, info, warn};
#[cfg(feature = "clustering")]
use waddle_xmpp::muc::room_actor::GetRoomClaimFence;
use waddle_xmpp::{
    carbons::CARBONS_NS,
    commands::{CommandContext, CommandResult},
    disco::{
        build_disco_info_response, build_disco_info_response_with_extensions,
        build_disco_items_response, community_service_features, muc_room_features,
        muc_service_features, parse_disco_info_query, parse_disco_items_query,
        push_service_features, spaces_service_features, upload_service_features, DiscoItem,
        Feature, Identity,
    },
    mam::{
        build_fin_iq, build_query_form_iq, build_result_messages, is_mam_query,
        is_mam_query_form_request, parse_mam_query, ArchivedMessage, ArchivedModeration,
        ArchivedRichMessage, ArchivedRichPayload, RichMessageId, RichText,
    },
    muc::{
        admin::{
            build_admin_result, build_admin_set_result, build_role_result, is_muc_admin_iq,
            is_role_change_query, parse_admin_query, AdminItem,
        },
        owner::build_config_form,
        room_actor::{
            ApplyAdminItems, ChangeAffiliation, CheckMutationOwnership, EnforceMembersOnly,
            EnforceMembersOnlyAffiliations, GetAdminContext, GetOccupantByJid, GetSnapshot,
            PingSelfCheck, RoomActor, UpdateConfig,
        },
        DATA_FORMS_NS,
    },
    presence::subscription::{
        build_subscription_presence, SubscriptionStateMachine, SubscriptionType,
    },
    protocol::{ConnectionPhase, StanzaContext as ProtocolStanzaContext},
    pubsub::{
        build_pep_identity, build_pubsub_affiliations_result, build_pubsub_configure_form_result,
        build_pubsub_error, build_pubsub_items_result, build_pubsub_owner_subscriptions_result,
        build_pubsub_publish_result, build_pubsub_subscribe_result, build_pubsub_success,
        is_pep_request, is_pep_request_to, is_pubsub_iq, parse_pubsub_iq, pep_features,
        PubSubError, PubSubItem, PubSubRequest, SubId, SubscriptionState, PEP_NODE_AVATAR_DATA,
        PEP_NODE_AVATAR_METADATA,
    },
    registry::BroadcastOutcome,
    roster::{
        build_roster_push, build_roster_result, build_roster_result_empty, parse_roster_get,
        parse_roster_set, AskType, RosterItem, RosterSetResult, RosterVersion, Subscription,
        ROSTER_NS,
    },
    xep::xep0357::{
        build_push_disable_result, build_push_enable_result, is_push_disable, is_push_enable,
        parse_push_disable, parse_push_enable,
    },
    xep::xep0363::{
        build_upload_error, build_upload_slot_response, effective_content_type, is_upload_request,
        parse_upload_request, sanitize_filename, UploadError, UploadSlot,
    },
    xep::xep0430::{
        build_inbox_entry_message, build_inbox_fin_iq, build_mark_read_result, is_inbox_iq,
        is_mark_read_iq, parse_inbox_query, parse_mark_read, InboxFinCounts, InboxLastMessage,
    },
    xep::{
        add_stanza_id_xep0359, build_command_items, build_command_result,
        build_last_activity_response, build_moderation_result_message,
        build_muc_slow_mode_roominfo_form, build_room_metadata_form,
        build_room_space_metadata_forms_with_description, build_search_form_response,
        build_search_response_with_rsm, build_server_role_form,
        build_spaces_metadata_form_for_requester_with_owners, is_last_activity_query,
        is_search_form_request, is_search_request, is_time_query, is_version_query,
        parse_command_from_iq, parse_moderation_iq, parse_search_request, AdHocCommandCondition,
        ChannelResult, Command, CommandError, CommandStatus, RsmRequest, RsmResponse, Searchable,
        SpaceAffiliation, Xep0359StanzaId, NODE_COMMANDS, NS_CHANNEL_SEARCH,
    },
    Affiliation, SpaceDetails, Stanza, StanzaErrorCondition, StanzaErrorType, XmppError,
};
use xmpp_parsers::minidom::Element;

mod archive_inbox_upload;
pub(crate) mod blocking;
mod caps_result;
mod commands;
mod community_items;
mod community_publish;
mod community_retract;
mod community_rsvp;
mod conn_state;
mod disco_info;
mod disco_items;
pub(crate) mod errors;
mod extension_forms;
mod extension_route_items;
mod full_jid_forward;
mod jingle_muji_gate;
mod last_activity;
mod link_preview_lookup;
pub(crate) mod link_preview_player_embed;
mod link_preview_resolver;
mod mentions_permissions;
mod muc_admin;
mod muc_owner_config;
mod muc_owner_moderation;
mod muc_self_ping;
mod pep_addressing;
mod permissions;
mod pin_query;
mod pubsub_admin;
mod pubsub_dispatch;
mod push;
pub(crate) mod roster;
mod sans_io;
mod search;
mod session_jid;
mod spaces_bookmark_cleanup;
mod spaces_discovery;
mod spaces_items;
mod spaces_publish;
mod spaces_retract;
mod story_attachments;
#[cfg(test)]
mod test_helpers;
mod vcard_private;

use archive_inbox_upload::handle_archive_inbox_upload_iq;
use blocking::handle_blocking_iq;
use caps_result::handle_caps_disco_info_result;
use commands::{handle_command_iq, CommandTargets};
use community_items::handle_community_items;
use community_publish::handle_community_publish;
use community_retract::handle_community_retract;
pub use conn_state::IqConnState;
use disco_info::handle_disco_info_iq;
use disco_items::handle_disco_items_iq;
use extension_forms::{
    command_name_by_boundary, command_refs_by_boundary, extension_command_metadata_form,
    extension_features_for_disco, extension_route_disco_node, extension_route_metadata_form,
    CommandBoundary, EXTENSION_COMMAND_FORM_TYPE, EXTENSION_ROUTE_FORM_TYPE,
};
use extension_route_items::{handle_extension_route_items, PubSubItemsRead};
use full_jid_forward::route_full_jid_iq;
use last_activity::handle_last_activity_iq;
use link_preview_lookup::{handle_link_preview_lookup_iq, is_link_preview_lookup_iq};
use mentions_permissions::{handle_mentions_permissions_iq, is_mentions_permissions_iq};
use muc_owner_config::apply_muc_owner_config;
use muc_owner_moderation::handle_muc_owner_and_moderation_iq;
use muc_self_ping::handle_muc_self_ping_iq;
use pep_addressing::is_pep_self_or_to;
pub(crate) use permissions::managed_channel_permission_allowed;
use permissions::{
    build_muc_owner_config_response, build_xmpp_error_response, global_database,
    muc_owner_authorized, server_affiliation_for_requester, space_affiliation_for_requester,
    spaces_node_mutation_allowed,
};
use pin_query::{handle_pin_query_iq, is_pin_query_iq};
use pubsub_dispatch::handle_pubsub_iq;
use spaces_discovery::{
    room_space_metadata_extensions, space_details_from_node, spaces_service_bare_jid,
};
use spaces_items::handle_spaces_items;
use spaces_publish::handle_spaces_publish;
use spaces_retract::handle_spaces_retract;
#[cfg(test)]
pub use test_helpers::handle_iq;
// Re-exported via `pub(super)` so submodules that wildcard-import the
// IQ-handler module's namespace (`use super::*;`) pick up the typed
// IQ-error constructors without each having to re-import the
// `errors` submodule directly.
pub(super) use errors::{
    bad_format_iq_error, bad_request_iq_error, conflict_iq_error, feature_not_implemented_iq_error,
    forbidden_iq_error, internal_server_error_iq_error, item_not_found_iq_error,
    jid_malformed_iq_error, not_acceptable_iq_error, not_allowed_iq_error, not_authorized_iq_error,
    service_unavailable_iq_error,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FencedRoomEffectsOutcome {
    Authorized,
    NotOwner,
    OwnershipUncertain,
}

/// Demote only the actor incarnation that produced the failed exact proof.
/// If E2 has already replaced retained E1 in the registry, the registry CAS
/// refuses to touch E2 and the direct kill still terminates the retained E1.
pub(crate) async fn demote_exact_room_actor(
    state: &WebSocketState,
    room_jid: &BareJid,
    room_actor: &ActorRef<RoomActor>,
) {
    let _ = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::DestroyRoomExact {
            room_jid: room_jid.clone(),
            expected_actor: room_actor.clone(),
        })
        .await;
    room_actor.kill();
}

/// Final local-incarnation half of an exact room-effects proof. Run this
/// after any backend claim check so a retained E1 cannot borrow E2's current
/// room-scoped authority merely because both use the same logical JID.
async fn fence_current_room_actor(
    state: &WebSocketState,
    room_jid: &BareJid,
    room_actor: &ActorRef<RoomActor>,
) -> FencedRoomEffectsOutcome {
    match state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
    {
        Ok(Some(current_actor)) if current_actor == *room_actor => {
            FencedRoomEffectsOutcome::Authorized
        }
        Ok(Some(_)) | Ok(None) => {
            demote_exact_room_actor(state, room_jid, room_actor).await;
            FencedRoomEffectsOutcome::NotOwner
        }
        Err(_) => FencedRoomEffectsOutcome::OwnershipUncertain,
    }
}

/// Final exact-incarnation proof for direct room effects that bypass the
/// sans-I/O room dispatcher (admin/config presence, moderation, destroy).
pub(super) async fn fence_room_effects(
    state: &WebSocketState,
    room_jid: &BareJid,
    room_actor: &ActorRef<RoomActor>,
) -> FencedRoomEffectsOutcome {
    #[cfg(feature = "clustering")]
    {
        let clustering = &state.deps.app_state.clustering_claims;
        if clustering.claim_store.is_none() {
            return fence_current_room_actor(state, room_jid, room_actor).await;
        }
        let Some(store) = clustering.muc_durable_store.as_ref() else {
            return FencedRoomEffectsOutcome::OwnershipUncertain;
        };
        let fence = match room_actor.ask(GetRoomClaimFence).await {
            Ok(Some(fence)) => fence,
            Ok(None) | Err(_) => return FencedRoomEffectsOutcome::OwnershipUncertain,
        };
        match store.check_fenced_fanout_exact(room_jid, &fence).await {
            Ok(true) => fence_current_room_actor(state, room_jid, room_actor).await,
            Ok(false) | Err(waddle_xmpp::XmppError::RoomOwnershipLost(_)) => {
                demote_exact_room_actor(state, room_jid, room_actor).await;
                FencedRoomEffectsOutcome::NotOwner
            }
            Err(_) => FencedRoomEffectsOutcome::OwnershipUncertain,
        }
    }
    #[cfg(not(feature = "clustering"))]
    {
        fence_current_room_actor(state, room_jid, room_actor).await
    }
}

#[cfg(all(test, feature = "clustering"))]
mod fenced_effect_tests {
    use super::*;
    use kameo::actor::Spawn;
    use std::sync::atomic::{AtomicBool, Ordering};
    use waddle_xmpp::muc::room_actor::{BindRoomClaimFence, GetSnapshot, Join};
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::muc::{DurableRoomState, MucDurableFuture, MucDurableStore, RoomConfig};
    use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};
    use waddle_xmpp_core::{Affiliation, Role};

    struct SwitchableFenceStore {
        owned: AtomicBool,
    }

    impl MucDurableStore for SwitchableFenceStore {
        fn load_room_state<'a>(
            &'a self,
            _room_jid: &'a BareJid,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            Box::pin(async { Ok(None) })
        }

        fn save_config<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _waddle_id: &'a str,
            _channel_id: &'a str,
            _config: &'a RoomConfig,
        ) -> MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn save_subject<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _subject: Option<&'a waddle_xmpp::muc::SubjectState>,
        ) -> MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn save_affiliation<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _entry: &'a waddle_xmpp::muc::affiliation::AffiliationEntry,
        ) -> MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn check_fenced_fanout_exact<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, bool> {
            let owned = self.owned.load(Ordering::SeqCst);
            Box::pin(async move { Ok(owned) })
        }
    }

    #[tokio::test]
    async fn ownership_loss_during_snapshot_and_scrub_window_suppresses_live_fanout() {
        use crate::server::routes::websocket::tests::{
            create_test_websocket_state_with_clustering, register_test_connection,
        };

        let store = Arc::new(SwitchableFenceStore {
            owned: AtomicBool::new(true),
        });
        let clustering = crate::clustering::ClusteringHandles {
            claim_store: Some(Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new())),
            muc_durable_store: Some(store.clone()),
            ..Default::default()
        };
        let sm_registry =
            Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
        let state = create_test_websocket_state_with_clustering(clustering, sm_registry).await;
        let room: BareJid = "late-fence@muc.example.com".parse().expect("room");
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "waddle".to_string(),
                channel_id: "channel".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room");
        actor
            .ask(BindRoomClaimFence {
                fence: waddle_xmpp::muc::RoomClaimFenceContext {
                    entity: Entity::new(EntityType::RoomActor, room.to_string()),
                    epoch: ClaimEpoch(1),
                    owner: NodeIdentity::new("node-a", "node-epoch-a"),
                },
            })
            .await
            .expect("bind E1");
        let occupant: jid::FullJid = "alice@example.com/web".parse().expect("occupant");
        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: occupant.clone(),
                role: Role::Moderator,
                affiliation: Affiliation::Owner,
            })
            .await
            .expect("join occupant");

        // The moderation path takes this snapshot and then awaits preview,
        // SM, and pending-delivery scrubs. Model the claim moving during
        // that window before the production final-effects helper runs.
        let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
        tokio::task::yield_now().await;
        store.owned.store(false, Ordering::SeqCst);

        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let _owner = register_test_connection(&state, &occupant, sender).await;
        if fence_room_effects(&state, &room, &actor).await == FencedRoomEffectsOutcome::Authorized {
            let presence =
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
            let _ = state.deps.protocol.connection_registry.try_send_to(
                &snapshot.room.occupants["alice"].real_jid,
                Stanza::Presence(presence),
            );
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn retained_actor_cannot_borrow_current_registry_incarnation_authority() {
        use crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering;

        let store = Arc::new(SwitchableFenceStore {
            owned: AtomicBool::new(true),
        });
        let clustering = crate::clustering::ClusteringHandles {
            claim_store: Some(Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new())),
            muc_durable_store: Some(store),
            ..Default::default()
        };
        let sm_registry =
            Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
        let state = create_test_websocket_state_with_clustering(clustering, sm_registry).await;
        let room: BareJid = "actor-ref-fence@muc.example.com".parse().expect("room");
        let current = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "current".to_string(),
                channel_id: "current".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create current actor");
        let retained = RoomActor::spawn(RoomActor::new(
            waddle_xmpp::muc::MucRoom::new(
                room.clone(),
                "retained".to_string(),
                "retained".to_string(),
                RoomConfig::default(),
            ),
            state.deps.occupant_id_secret.clone(),
        ));
        retained
            .ask(BindRoomClaimFence {
                fence: waddle_xmpp::muc::RoomClaimFenceContext {
                    entity: Entity::new(EntityType::RoomActor, room.to_string()),
                    epoch: ClaimEpoch(1),
                    owner: NodeIdentity::new("node-a", "node-epoch-a"),
                },
            })
            .await
            .expect("bind retained fence");

        assert_eq!(
            fence_room_effects(&state, &room, &retained).await,
            FencedRoomEffectsOutcome::NotOwner
        );
        tokio::task::yield_now().await;
        assert!(!retained.is_alive());
        let still_current = state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::GetRoom { room_jid: room })
            .await
            .expect("get current actor")
            .expect("current actor remains");
        assert_eq!(still_current, current);
        assert!(current.is_alive());
    }
}

fn push_service_stanza_error(error: XmppError) -> xmpp_parsers::stanza_error::StanzaError {
    match error {
        XmppError::Stanza {
            condition: StanzaErrorCondition::BadRequest,
            ..
        } => bad_request_iq_error("Malformed Push Service request."),
        XmppError::Stanza {
            condition: StanzaErrorCondition::ItemNotFound,
            ..
        } => item_not_found_iq_error("Requested Push Service item not found."),
        XmppError::Stanza {
            condition: StanzaErrorCondition::Forbidden,
            ..
        }
        | XmppError::PermissionDenied(_) => forbidden_iq_error("Push Service request forbidden."),
        _ => internal_server_error_iq_error("Internal server error."),
    }
}
use muc_admin::handle_muc_admin_iq;
use push::handle_push_iq;
use roster::handle_roster_iq;
use sans_io::handle_sans_io_iq;
use search::{handle_channel_search_iq, handle_user_search_iq};
use vcard_private::{handle_private_storage_iq, handle_vcard_iq};

// `build_iq_error_xml_typed` is re-exported via `pub(super) use` so
// submodules wildcard-importing this module's namespace see it.
pub(super) use super::super::build_iq_error_xml_typed;

use super::super::{
    build_iq_result_xml, element_to_xml, get_room_actor, iq_to_xml, is_muc_room_jid, stanza_to_xml,
    WebSocketState,
};
use super::presence::{
    get_managed_channel_for_room, resolve_muc_room_archive_access,
    send_current_presence_from_user_to_jid, send_unavailable_presence_from_user_to_jid,
    RoomArchiveAccess,
};
use crate::auth::{local_account_exists, Session};
use crate::db::actor::{DbExecute, DbQuery, DbQueryOne, GetDatabase};
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::roster::{
    DatabaseRosterStorage, RosterItemRow, RosterRowChange, RosterRowMutationKind,
    RosterStorageError,
};
use crate::db::{row_value, Database, Value, ValueExt};
use crate::permissions::{
    CheckPermission, DeleteTuple, Object, ObjectType, Permission, PermissionError, Relation,
    Subject, SubjectType, Tuple, WriteTuple,
};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use crate::server::managed_channel_policy::{
    server_policy_for_managed_channel, ManagedChannelServerPolicy,
    DEPLOYMENT_MEMBERSHIP_PERMISSIONS,
};
use crate::server::xmpp_state::{get_xmpp_channel, list_xmpp_channels, XmppChannelRecord};
use crate::vcard::VCardStore;

#[derive(Clone, Copy)]
pub(super) struct IqHandlerContext<'a> {
    pub(super) iq: &'a xmpp_parsers::iq::Iq,
    pub(super) id: &'a str,
    pub(super) payload_ns: &'a str,
    pub(super) target_to: Option<&'a str>,
    pub(super) has_destroy: bool,
    pub(super) domain: &'a str,
    pub(super) muc_domain: &'a str,
    pub(super) upload_domain: &'a str,
    pub(super) spaces_domain: &'a str,
    pub(super) community_domain: &'a str,
    pub(super) extensions_domain: &'a str,
    pub(super) push_domain: &'a str,
    pub(super) response_from: Option<&'a str>,
    pub(super) response_to: Option<&'a str>,
}

pub async fn handle_iq_with_conn_state(
    iq: xmpp_parsers::iq::Iq,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    phase: &ConnectionPhase,
    conn_state: &mut IqConnState<'_>,
) -> Vec<String> {
    let spaces_domain = state.deps.service_domains.spaces.clone();
    let community_domain = state.deps.service_domains.community.clone();
    let upload_domain = state.deps.service_domains.upload.clone();
    let extensions_domain = state.deps.service_domains.extensions.clone();
    let push_domain = state.deps.service_domains.push.clone();

    let id = iq.id().to_string();
    let to = iq.to().map(|jid| jid.to_string());
    let from = iq.from().map(|jid| jid.to_string());
    let response_from = to.as_deref();
    let response_to = from.as_deref();

    if matches!(
        &iq,
        xmpp_parsers::iq::Iq::Result { .. } | xmpp_parsers::iq::Iq::Error { .. }
    ) {
        if let Some(full) = phase.bound_jid() {
            handle_caps_disco_info_result(&iq, full, state);
        }
        debug!(id = %id, "Ignoring IQ result/error stanza");
        return vec![];
    }

    let payload_ns = match &iq {
        xmpp_parsers::iq::Iq::Get { payload: e, .. }
        | xmpp_parsers::iq::Iq::Set { payload: e, .. } => e.ns(),
        _ => String::new(),
    };
    let has_destroy = match &iq {
        xmpp_parsers::iq::Iq::Set { payload: e, .. } => e
            .get_child("destroy", "http://jabber.org/protocol/muc#owner")
            .is_some(),
        _ => false,
    };

    if waddle_xmpp::xep::xep0410::is_self_ping(&iq)
        && iq.to().is_some_and(|to| to.domain().as_str() == muc_domain)
    {
        return handle_muc_self_ping_iq(&iq, state, phase.bound_jid(), response_from, response_to)
            .await;
    }

    if is_link_preview_lookup_iq(&iq) {
        return handle_link_preview_lookup_iq(
            &iq,
            phase.bound_jid(),
            state,
            muc_domain,
            response_from,
            response_to,
            state.deps.occupant_id_secret.key(),
        )
        .await;
    }

    if payload_ns == ROSTER_NS {
        return handle_roster_iq(
            &iq,
            domain,
            state,
            phase.bound_jid(),
            conn_state.roster_interested,
            conn_state.registry_owner,
        )
        .await;
    }

    // Namespace-based dispatch takes priority over the generic
    // full-jid forwarder ONLY for the handful of namespaces whose
    // server-side handler must mediate the stanza before it reaches
    // the peer — e.g. XEP-0166 Jingle stamping a LiveKit join token
    // onto the Waddle transport in the rewrite path. For other
    // handler-registered namespaces (XEP-0199 ping, XEP-0030
    // disco#info, etc.) the historical contract is that IQs
    // addressed to another user's full JID are forwarded verbatim
    // and answered by that user's real client, not by our local
    // handler — preserve that.
    let payload_mediates_peer_routing = matches!(
        payload_ns.as_str(),
        s if s == waddle_xmpp::xep::xep0166::NS_JINGLE
            || s == waddle_xmpp::xep::xep0215::NS_EXT_DISCO
    );
    if !payload_mediates_peer_routing {
        if let Some(target) = iq.to().and_then(|jid| jid.clone().try_into_full().ok()) {
            if target.domain().as_str() == domain {
                return route_full_jid_iq(
                    iq,
                    state,
                    phase.bound_jid(),
                    target,
                    response_from,
                    conn_state.ordered_relay_origin.clone(),
                )
                .await;
            }
        }
    }

    let handler_ctx = IqHandlerContext {
        iq: &iq,
        id: id.as_str(),
        payload_ns: payload_ns.as_str(),
        target_to: to.as_deref(),
        has_destroy,
        domain,
        muc_domain,
        upload_domain: upload_domain.as_str(),
        spaces_domain: spaces_domain.as_str(),
        community_domain: community_domain.as_str(),
        extensions_domain: extensions_domain.as_str(),
        push_domain: push_domain.as_str(),
        response_from,
        response_to,
    };

    // `Some(_)` means a registered dispatcher handler owned this IQ;
    // it is terminal even when the frame list is empty (e.g. a Jingle
    // 1:1 stanza forwarded to the peer with no synchronous reply for
    // the sender). Only `None` — no handler claimed the namespace —
    // continues to the remaining branches below.
    if let Some(frames) =
        handle_sans_io_iq(handler_ctx, state, authenticated_session, phase, conn_state).await
    {
        return frames;
    }

    // jabber:iq:roster is served by handle_roster_iq above because it needs
    // durable roster storage and roster-push fanout.
    let misc_response = if waddle_xmpp::xep::xep0054::is_vcard_get(&iq)
        || waddle_xmpp::xep::xep0054::is_vcard_set(&iq)
    {
        handle_vcard_iq(&iq, state, phase.bound_jid(), response_from, response_to).await
    } else if is_last_activity_query(&iq) {
        handle_last_activity_iq(
            &iq,
            domain,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
        )
        .await
    } else if waddle_xmpp::xep::xep0049::is_private_storage_query(&iq) {
        handle_private_storage_iq(&iq, state, phase.bound_jid(), response_from, response_to).await
    } else if waddle_xmpp::xep::xep0191::is_blocking_query(&iq) {
        handle_blocking_iq(
            &iq,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
            conn_state,
        )
        .await
    } else if is_push_enable(&iq) || is_push_disable(&iq) {
        handle_push_iq(
            &iq,
            state,
            phase.bound_jid(),
            push_domain.as_str(),
            response_from,
            response_to,
        )
        .await
    } else if is_search_request(&iq) {
        handle_channel_search_iq(&iq, muc_domain, state, response_from, response_to).await
    } else if payload_ns == "jabber:iq:search" {
        handle_user_search_iq(&iq, domain, state, response_from, response_to).await
    } else {
        Vec::new()
    };
    if !misc_response.is_empty() {
        return misc_response;
    }

    let disco_info_response =
        handle_disco_info_iq(handler_ctx, state, phase, authenticated_session).await;
    if !disco_info_response.is_empty() {
        return disco_info_response;
    }

    let disco_items_response = handle_disco_items_iq(handler_ctx, state, phase).await;
    if !disco_items_response.is_empty() {
        return disco_items_response;
    }

    if payload_ns == "http://jabber.org/protocol/commands" {
        return handle_command_iq(
            &iq,
            state,
            CommandTargets {
                domain,
                muc_domain,
                extensions_domain: &extensions_domain,
                push_domain: &push_domain,
            },
            authenticated_session,
            phase.bound_jid(),
        )
        .await;
    }

    if payload_ns == "http://jabber.org/protocol/muc#admin" && is_muc_admin_iq(&iq, muc_domain) {
        return handle_muc_admin_iq(
            &iq,
            muc_domain,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
        )
        .await;
    }

    if is_pin_query_iq(&iq, muc_domain, domain) {
        return handle_pin_query_iq(&iq, state, phase.bound_jid(), response_from, response_to)
            .await;
    }

    if is_mentions_permissions_iq(&iq, muc_domain) {
        return handle_mentions_permissions_iq(
            &iq,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
        )
        .await;
    }

    let muc_owner_or_moderation =
        handle_muc_owner_and_moderation_iq(handler_ctx, state, phase, authenticated_session).await;
    if !muc_owner_or_moderation.is_empty() {
        return muc_owner_or_moderation;
    }

    let archive_inbox_upload =
        handle_archive_inbox_upload_iq(&iq, &id, payload_ns.as_str(), domain, state, phase).await;
    if !archive_inbox_upload.is_empty() {
        return archive_inbox_upload;
    }

    let pubsub_response = handle_pubsub_iq(handler_ctx, state, phase, authenticated_session).await;
    if !pubsub_response.is_empty() {
        return pubsub_response;
    }

    // Unknown IQ - log a compact summary and return an error.
    let payload_ns = (!payload_ns.is_empty()).then_some(payload_ns.as_str());
    warn!(id = %id, payload_ns, "Unhandled IQ stanza");
    vec![build_iq_error_xml_typed(
        &id,
        response_from,
        response_to,
        feature_not_implemented_iq_error("Requested feature not implemented."),
    )]
}

#[cfg(test)]
mod tests;
