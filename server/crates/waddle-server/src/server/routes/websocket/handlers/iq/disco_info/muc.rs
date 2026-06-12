use super::*;

/// XEP-0030 routing classification for a disco#info target as seen by the
/// MUC dispatcher.
///
/// The MUC handler must only consult the `RoomRegistryActor` for targets that
/// are actually hosted on the MUC service domain. Any other target (a sibling
/// component such as `upload.<domain>`, or a JID on a non-MUC domain) is
/// `NotMuc` and the handler defers to the next handler **without** touching the
/// room registry — see #757, where a wedged `RoomRegistryActor` froze
/// disco#info for every component because the MUC handler looked rooms up
/// unconditionally.
#[derive(Debug, PartialEq)]
enum MucDiscoTarget {
    /// The MUC service JID itself (`muc.<domain>`).
    Service,
    /// A room hosted on the MUC service (`<node>@muc.<domain>`).
    Room(BareJid),
    /// Not a MUC target — the dispatcher returns `None`.
    NotMuc,
}

/// Decide how the MUC dispatcher should treat a disco#info target, without any
/// I/O. Pure so the routing decision is verifiable in isolation.
fn classify_muc_disco_target(target_to: Option<&str>, muc_domain: &str) -> MucDiscoTarget {
    if target_to == Some(muc_domain) {
        return MucDiscoTarget::Service;
    }
    let Some(target) = target_to else {
        return MucDiscoTarget::NotMuc;
    };
    let room_target = target.split('/').next().unwrap_or(target);
    let Ok(room_jid) = room_target.parse::<BareJid>() else {
        return MucDiscoTarget::NotMuc;
    };
    if room_jid.domain().as_str() != muc_domain {
        return MucDiscoTarget::NotMuc;
    }
    MucDiscoTarget::Room(room_jid)
}

pub(super) async fn handle_muc_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
) -> Option<DiscoInfoResponse<'a>> {
    // #757: only consult the RoomRegistryActor for targets actually on the
    // MUC service domain. A sibling component (`upload.<domain>`, …) or a JID
    // on any non-MUC domain is `NotMuc` and we defer to the next handler
    // without any room-registry I/O — otherwise a wedged actor freezes
    // disco#info for every component, not just MUC.
    let room_jid = match classify_muc_disco_target(req.target_to, req.muc_domain) {
        MucDiscoTarget::Service => {
            let identities = vec![Identity::muc_service(Some("Waddle Chatrooms"))];
            let mut features = muc_service_features();
            features.push(Feature::replies());
            features.push(Feature::new(NS_CHANNEL_SEARCH));
            features.extend(extension_features_for_disco(state));
            let response = build_disco_info_response(req.request_iq, &identities, &features, None);
            return Some(DiscoInfoResponse::iq(response));
        }
        MucDiscoTarget::NotMuc => return None,
        MucDiscoTarget::Room(room_jid) => room_jid,
    };

    if let Some(room_actor) = get_room_actor(state, &room_jid).await {
        let snapshot = match room_actor.ask(GetSnapshot).await {
            Ok(snapshot) => snapshot.room,
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "Failed to load room snapshot for disco#info"
                );
                return Some(DiscoInfoResponse::error(
                    req.id,
                    req.response_from,
                    req.response_to,
                    internal_server_error_iq_error("Internal server error."),
                ));
            }
        };
        let managed_channel = get_managed_channel_for_room(state, &room_jid)
            .await
            .ok()
            .flatten();
        let channel_type = managed_channel
            .as_ref()
            .map(|channel| channel.channel_type.as_str())
            .unwrap_or(if snapshot.config.forum {
                "forum"
            } else {
                "text"
            });
        let description = managed_channel
            .as_ref()
            .and_then(|channel| channel.description.as_deref())
            .or(snapshot.config.description.as_deref());
        let identities = vec![Identity::muc_room(Some(&snapshot.config.name))];
        let mut features = muc_room_features(
            snapshot.config.persistent,
            snapshot.config.members_only,
            snapshot.config.moderated || channel_type == "announcement",
            snapshot.config.forum || channel_type == "forum",
        );
        if snapshot.config.group_dm {
            features.push(Feature::new(waddle_xmpp::admin::NS_GROUP_DM_FEATURE));
        }
        features.extend(extension_features_for_disco(state));
        let mut extensions = room_space_metadata_extensions(state, &room_jid, description).await;
        let has_space_metadata = !extensions.is_empty();
        if has_space_metadata {
            features.push(Feature::spaces());
        }
        extensions.push(build_room_metadata_form(
            channel_type,
            snapshot.config.pin_permission.as_form_value(),
        ));
        let response = build_disco_info_response_with_extensions(
            req.request_iq,
            &identities,
            &features,
            None,
            &extensions,
        );
        return Some(DiscoInfoResponse::iq(response));
    }

    if !is_muc_room_jid(state, &room_jid).await {
        return None;
    }

    if let Ok(Some(channel)) = get_managed_channel_for_room(state, &room_jid).await {
        let identities = vec![Identity::muc_room(Some(&channel.name))];
        let mut features = muc_room_features(
            true,
            channel.members_only,
            channel.channel_type == "announcement",
            channel.channel_type == "forum",
        );
        features.extend(extension_features_for_disco(state));
        let mut extensions =
            room_space_metadata_extensions(state, &room_jid, channel.description.as_deref()).await;
        let has_space_metadata = !extensions.is_empty();
        if has_space_metadata {
            features.push(Feature::spaces());
        }
        // #422: read the persisted pin policy from the channel record so
        // dormant rooms advertise the truth, not the default.
        extensions.push(build_room_metadata_form(
            &channel.channel_type,
            channel.pin_permission.as_form_value(),
        ));
        let response = build_disco_info_response_with_extensions(
            req.request_iq,
            &identities,
            &features,
            None,
            &extensions,
        );
        return Some(DiscoInfoResponse::iq(response));
    }

    let room_name = room_jid
        .node()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "Room".to_string());
    let identities = vec![Identity::muc_room(Some(&room_name))];
    let mut features = muc_room_features(false, false, false, false);
    features.extend(extension_features_for_disco(state));
    let response = build_disco_info_response(req.request_iq, &identities, &features, None);
    Some(DiscoInfoResponse::iq(response))
}

#[cfg(test)]
mod tests {
    use super::{classify_muc_disco_target, MucDiscoTarget};

    const MUC_DOMAIN: &str = "muc.test.local";

    #[test]
    fn bare_muc_service_domain_is_service() {
        assert_eq!(
            classify_muc_disco_target(Some(MUC_DOMAIN), MUC_DOMAIN),
            MucDiscoTarget::Service
        );
    }

    #[test]
    fn room_on_muc_domain_is_room() {
        let target = "general@muc.test.local";
        assert_eq!(
            classify_muc_disco_target(Some(target), MUC_DOMAIN),
            MucDiscoTarget::Room(target.parse().expect("valid room jid"))
        );
    }

    #[test]
    fn room_with_resource_strips_to_bare_room() {
        assert_eq!(
            classify_muc_disco_target(Some("general@muc.test.local/alice"), MUC_DOMAIN),
            MucDiscoTarget::Room("general@muc.test.local".parse().expect("valid room jid"))
        );
    }

    // #757: a sibling component domain must NOT be routed through the MUC
    // handler — otherwise its disco#info is coupled to RoomRegistryActor health.
    #[test]
    fn sibling_component_domain_is_not_muc() {
        assert_eq!(
            classify_muc_disco_target(Some("upload.test.local"), MUC_DOMAIN),
            MucDiscoTarget::NotMuc
        );
    }

    // #757: a node JID hosted on a non-MUC domain must not reach the room
    // registry either — the MUC handler only owns the MUC service domain.
    #[test]
    fn node_jid_on_other_domain_is_not_muc() {
        assert_eq!(
            classify_muc_disco_target(Some("room@chat.test.local"), MUC_DOMAIN),
            MucDiscoTarget::NotMuc
        );
    }

    #[test]
    fn absent_target_is_not_muc() {
        assert_eq!(
            classify_muc_disco_target(None, MUC_DOMAIN),
            MucDiscoTarget::NotMuc
        );
    }
}
