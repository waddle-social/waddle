use super::permissions::write_tuple_if_absent;
use super::*;
use crate::admin::channels::{acquire_room_config_lock, explicit_channel_affiliations_for_jids};

fn data_form_value(form: &Element, var: &str) -> Option<String> {
    form.children()
        .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
        .find(|field| field.attr("var") == Some(var))
        .and_then(|field| field.get_child("value", DATA_FORMS_NS))
        .map(|value| value.texts().collect())
}

fn data_form_bool(form: &Element, var: &str) -> Option<bool> {
    data_form_value(form, var).and_then(|value| match value.as_str() {
        "1" | "true" | "True" | "TRUE" => Some(true),
        "0" | "false" | "False" | "FALSE" => Some(false),
        _ => None,
    })
}

fn channel_type_from_config(config: &waddle_xmpp::muc::RoomConfig) -> waddle_xmpp::ChannelType {
    if config.group_dm {
        waddle_xmpp::ChannelType::GroupDm
    } else if config.forum {
        waddle_xmpp::ChannelType::Forum
    } else if config.moderated {
        waddle_xmpp::ChannelType::Announcement
    } else {
        waddle_xmpp::ChannelType::Text
    }
}

pub(super) fn project_channel_type_to_config(
    config: &mut waddle_xmpp::muc::RoomConfig,
    channel_type: waddle_xmpp::ChannelType,
) {
    match channel_type {
        waddle_xmpp::ChannelType::Text => {
            config.group_dm = false;
            config.forum = false;
        }
        waddle_xmpp::ChannelType::Announcement => {
            config.group_dm = false;
            config.forum = false;
            config.moderated = true;
        }
        waddle_xmpp::ChannelType::Forum => {
            config.group_dm = false;
            config.forum = true;
        }
        waddle_xmpp::ChannelType::GroupDm => {
            config.group_dm = true;
            config.forum = false;
            config.members_only = true;
        }
    }
}

pub(super) async fn managed_channel_type_for_room(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<Option<waddle_xmpp::ChannelType>, String> {
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
        return Ok(None);
    };
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    get_xmpp_channel(actor, &channel_id)
        .await
        .map_err(|error| format!("channel lookup failed: {error}"))
        .map(|channel| {
            channel.and_then(|channel| waddle_xmpp::ChannelType::parse(&channel.channel_type))
        })
}

pub(super) async fn apply_muc_owner_config(
    state: &WebSocketState,
    room_jid: &BareJid,
    iq: &xmpp_parsers::iq::Iq,
    session: Option<&Session>,
) -> Result<(), String> {
    let room_actor = get_room_actor(state, room_jid)
        .await
        .ok_or_else(|| "room actor not found".to_string())?;
    let _config_guard = acquire_room_config_lock(room_jid).await;
    let snapshot = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| format!("snapshot failed: {error:?}"))?;
    let previous_members_only = snapshot.room.config.members_only;
    let previous_config = snapshot.room.config.clone();
    let occupant_jids: Vec<BareJid> = snapshot
        .room
        .occupants
        .values()
        .map(|occupant| occupant.real_jid.to_bare())
        .collect();
    let mut config = previous_config.clone();

    if let xmpp_parsers::iq::Iq::Set { payload: query, .. } = iq {
        if let Some(form) = query.get_child("x", DATA_FORMS_NS) {
            if let Some(name) =
                data_form_value(form, "muc#roomconfig_roomname").filter(|value| !value.is_empty())
            {
                config.name = name;
            }
            // Treat presence of the roomdesc field as authoritative
            // even when empty, so an owner can clear the description.
            // Field absent => keep existing; field present + empty
            // => clear; field present + non-empty => set.
            if let Some(value) = data_form_value(form, "muc#roomconfig_roomdesc") {
                config.description = if value.is_empty() { None } else { Some(value) };
            }
            if let Some(members_only) = data_form_bool(form, "muc#roomconfig_membersonly") {
                config.members_only = members_only;
            }
            if let Some(public_room) = data_form_bool(form, "muc#roomconfig_publicroom") {
                config.public_room = public_room;
            }
            if let Some(moderated) = data_form_bool(form, "muc#roomconfig_moderatedroom") {
                config.moderated = moderated;
            }
            if let Some(enable_logging) = data_form_bool(form, "muc#roomconfig_enablelogging") {
                config.enable_logging = enable_logging;
            }
            // XEP-0045 §8.1 (#1265 item 8): who may change the subject.
            if let Some(changesubject) = data_form_bool(form, "muc#roomconfig_changesubject") {
                config.occupants_may_change_subject = changesubject;
            }
            if let Some(forum) = data_form_bool(form, "muc#roomconfig_forum") {
                config.forum = forum;
            }
            // XEP-0045 muc#roomconfig_allowpm (#1257): who may send
            // §7.5 private messages. Unknown values keep the previous
            // policy (mirrors the pin-permission fallback rule).
            if let Some(value) = data_form_value(form, "muc#roomconfig_allowpm") {
                if let Some(allow_pm) = waddle_xmpp::muc::AllowPm::from_form_value(&value) {
                    config.allow_pm = allow_pm;
                }
            }
            // #415: per-room pin permission policy.
            if let Some(value) =
                data_form_value(form, waddle_xmpp::muc::owner::FIELD_PIN_PERMISSION)
            {
                if let Some(pin_permission) =
                    waddle_xmpp::muc::PinPermission::from_form_value(&value)
                {
                    config.pin_permission = pin_permission;
                }
            }
            // #1265 item 7: honor muc#roomconfig_maxusers on submit.
            // The registrar value "none" and 0 both mean unlimited.
            if let Some(value) = data_form_value(form, "muc#roomconfig_maxusers") {
                let trimmed = value.trim();
                if trimmed.eq_ignore_ascii_case("none") {
                    config.max_occupants = 0;
                } else if let Ok(max_occupants) = trimmed.parse::<u32>() {
                    config.max_occupants = max_occupants;
                }
            }
        }
    }

    // #1265 item 7: `muc#roomconfig_persistentroom` is not offered in
    // the config form — submitting this very form persists the room
    // into the channel catalog below, so every configured room is
    // persistent by construction. A submitted value for the unoffered
    // field is ignored.
    config.persistent = true;

    let channel_id = waddle_xmpp::parse_managed_room_jid(room_jid);
    let existing_channel_type = managed_channel_type_for_room(state, room_jid).await?;
    if let Some(channel_type) = existing_channel_type {
        project_channel_type_to_config(&mut config, channel_type);
    }
    if let (Some(channel_id), Some(session)) = (channel_id.as_deref(), session) {
        write_tuple_if_absent(
            state,
            Tuple::new(
                Object::new(ObjectType::Channel, channel_id),
                Relation::new("owner"),
                Subject::user(&session.user_jid),
            ),
        )
        .await
        .map_err(|error| format!("channel owner tuple failed: {error}"))?;
    }
    let managed_enforcement_affiliations = if !previous_members_only && config.members_only {
        if let Some(channel_id) = channel_id.as_deref() {
            Some(
                explicit_channel_affiliations_for_jids(
                    &state.deps.app_state,
                    channel_id,
                    occupant_jids,
                )
                .await?,
            )
        } else {
            None
        }
    } else {
        None
    };

    let expected_revision = room_actor
        .ask(UpdateConfig {
            config: config.clone(),
        })
        .await
        .map_err(|error| format!("config update failed: {error:?}"))?;
    let config_status_codes =
        waddle_xmpp::muc::config_change_status_codes(&previous_config, &config);

    let Some(channel_id) = channel_id else {
        if !previous_members_only && config.members_only {
            let updates = room_actor
                .ask(EnforceMembersOnly)
                .await
                .map_err(|error| format!("members-only enforcement failed: {error:?}"))?;
            for (recipient, presence) in updates {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&recipient, Stanza::Presence(presence));
            }
        }
        let post_update_snapshot = room_actor
            .ask(GetSnapshot)
            .await
            .map_err(|error| format!("post-config snapshot failed: {error:?}"))?;
        broadcast_muc_config_change(
            state,
            room_jid,
            &post_update_snapshot.room,
            &config_status_codes,
        );
        return Ok(());
    };

    let now = chrono::Utc::now().to_rfc3339();
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let channel_type = existing_channel_type.unwrap_or_else(|| channel_type_from_config(&config));
    if let Err(error) = actor
        .ask(DbExecute {
            sql: r#"
                INSERT INTO channels (id, name, description, channel_type, position, is_default, pin_permission, members_only, public_room, created_at, updated_at)
                VALUES (?, ?, ?, ?, 0, 0, ?, ?, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    description = excluded.description,
                    channel_type = excluded.channel_type,
                    pin_permission = excluded.pin_permission,
                    members_only = excluded.members_only,
                    public_room = excluded.public_room,
                    updated_at = excluded.updated_at
            "#
            .to_string(),
            params: vec![
                channel_id.clone().into(),
                config.name.clone().into(),
                config.description.clone().into(),
                channel_type.as_str().into(),
                config.pin_permission.as_form_value().into(),
                config.members_only.into(),
                config.public_room.into(),
                now.clone().into(),
                now.into(),
            ],
        })
        .await
    {
        let _ = room_actor
            .ask(waddle_xmpp::muc::room_actor::RollbackConfigIfRevision {
                expected_revision,
                config: previous_config,
            })
            .await;
        return Err(format!("channel upsert failed: {error}"));
    }

    // The channel#owner tuple was written before the config update/upsert so the
    // creator can always rejoin after restart, before a Space bookmark is
    // published. XEP-0045 §10 requires the room creator to be an owner.
    if session.is_none() {
        warn!(
            channel_id = %channel_id,
            "apply_muc_owner_config called without a session; \
             channel owner tuple not written — room may be inaccessible after server restart"
        );
    }

    if !previous_members_only && config.members_only {
        let updates = if let Some(affiliations) = managed_enforcement_affiliations {
            room_actor
                .ask(EnforceMembersOnlyAffiliations { affiliations })
                .await
                .map_err(|error| format!("members-only enforcement failed: {error:?}"))?
        } else {
            room_actor
                .ask(EnforceMembersOnly)
                .await
                .map_err(|error| format!("members-only enforcement failed: {error:?}"))?
        };
        for (recipient, presence) in updates {
            let _ = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(&recipient, Stanza::Presence(presence));
        }
    }

    let post_update_snapshot = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| format!("post-config snapshot failed: {error:?}"))?;
    broadcast_muc_config_change(
        state,
        room_jid,
        &post_update_snapshot.room,
        &config_status_codes,
    );

    Ok(())
}

fn broadcast_muc_config_change(
    state: &WebSocketState,
    room_jid: &BareJid,
    room: &waddle_xmpp::muc::MucRoom,
    status_codes: &[waddle_xmpp::muc::MucConfigStatusCode],
) {
    if status_codes.is_empty() {
        return;
    }

    for occupant in room.occupants.values() {
        for recipient_jid in room.get_occupant_sessions(&occupant.nick) {
            let message = waddle_xmpp::muc::build_config_change_message(
                room_jid,
                &recipient_jid,
                status_codes,
            );
            let _ = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(&recipient_jid, Stanza::Message(message));
        }
    }
}

#[cfg(test)]
mod channel_type_projection_tests {
    use super::*;

    #[test]
    fn text_projection_preserves_xep_moderatedroom() {
        let mut config = waddle_xmpp::muc::RoomConfig {
            group_dm: true,
            forum: true,
            moderated: true,
            ..waddle_xmpp::muc::RoomConfig::default()
        };

        project_channel_type_to_config(&mut config, waddle_xmpp::ChannelType::Text);

        assert!(!config.group_dm);
        assert!(!config.forum);
        assert!(config.moderated);
    }

    #[test]
    fn announcement_projection_forces_moderatedroom() {
        let mut config = waddle_xmpp::muc::RoomConfig {
            forum: true,
            moderated: false,
            ..waddle_xmpp::muc::RoomConfig::default()
        };

        project_channel_type_to_config(&mut config, waddle_xmpp::ChannelType::Announcement);

        assert!(!config.group_dm);
        assert!(!config.forum);
        assert!(config.moderated);
    }

    #[test]
    fn group_dm_projection_forces_members_only() {
        let mut config = waddle_xmpp::muc::RoomConfig {
            members_only: false,
            ..waddle_xmpp::muc::RoomConfig::default()
        };

        project_channel_type_to_config(&mut config, waddle_xmpp::ChannelType::GroupDm);

        assert!(config.group_dm);
        assert!(config.members_only);
    }
}
