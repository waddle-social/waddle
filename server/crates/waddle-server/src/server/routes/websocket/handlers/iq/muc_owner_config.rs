use super::permissions::write_tuple_if_absent;
use super::*;

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

pub(super) async fn apply_muc_owner_config(
    state: &WebSocketState,
    room_jid: &BareJid,
    iq: &xmpp_parsers::iq::Iq,
    session: Option<&Session>,
) -> Result<(), String> {
    let room_actor = get_room_actor(state, room_jid)
        .await
        .ok_or_else(|| "room actor not found".to_string())?;
    let mut config = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| format!("snapshot failed: {error:?}"))?
        .room
        .config;

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
            if let Some(forum) = data_form_bool(form, "muc#roomconfig_forum") {
                config.forum = forum;
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
        }
    }

    // Waddle rooms are persistent, non-anonymous collaboration surfaces.
    config.persistent = true;

    room_actor
        .ask(UpdateConfig {
            config: config.clone(),
        })
        .await
        .map_err(|error| format!("config update failed: {error:?}"))?;

    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
        return Ok(());
    };
    let now = chrono::Utc::now().to_rfc3339();
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let existing_channel_type = get_xmpp_channel(actor.clone(), &channel_id)
        .await
        .map_err(|error| format!("channel lookup failed: {error}"))?
        .map(|channel| channel.channel_type);
    let channel_type = if config.forum {
        "forum".to_string()
    } else if existing_channel_type.as_deref() == Some("announcement") {
        "announcement".to_string()
    } else {
        "text".to_string()
    };
    actor
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
                config.name.into(),
                config.description.into(),
                channel_type.into(),
                config.pin_permission.as_form_value().into(),
                config.members_only.into(),
                config.public_room.into(),
                now.clone().into(),
                now.into(),
            ],
        })
        .await
        .map_err(|error| format!("channel upsert failed: {error}"))?;

    // Write channel#owner → session user so the creator can always rejoin the
    // managed room after a server restart (before a Space bookmark is published).
    // XEP-0045 §10 requires the room creator to be an owner; without this tuple
    // the channel becomes unjoinable after restart.
    match session {
        Some(session) => {
            write_tuple_if_absent(
                state,
                Tuple::new(
                    Object::new(ObjectType::Channel, &channel_id),
                    Relation::new("owner"),
                    Subject::user(&session.user_id),
                ),
            )
            .await
            .map_err(|error| format!("channel owner tuple failed: {error}"))?;
        }
        None => {
            warn!(
                channel_id = %channel_id,
                "apply_muc_owner_config called without a session; \
                 channel owner tuple not written — room may be inaccessible after server restart"
            );
        }
    }

    Ok(())
}
