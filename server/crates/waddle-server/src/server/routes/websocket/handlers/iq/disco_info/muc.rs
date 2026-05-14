use super::*;

pub(super) async fn handle_muc_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
) -> Option<DiscoInfoResponse<'a>> {
    if req.target_to == Some(req.muc_domain) {
        let identities = vec![Identity::muc_service(Some("Waddle Chatrooms"))];
        let mut features = vec![
            Feature::muc(),
            Feature::replies(),
            Feature::new(NS_CHANNEL_SEARCH),
        ];
        features.extend(extension_features_for_disco(state));
        let response = build_disco_info_response(req.request_iq, &identities, &features, None);
        return Some(DiscoInfoResponse::iq(response));
    }

    let target = req.target_to?;
    let room_target = target.split('/').next().unwrap_or(target);
    let room_jid = room_target.parse::<BareJid>().ok()?;

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
            true,
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
