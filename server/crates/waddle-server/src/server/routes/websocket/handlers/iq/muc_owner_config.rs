use super::permissions::write_tuple_if_absent;
use super::*;

const MAX_MENTION_PERMISSIONS_COUNT: u32 = i32::MAX as u32;
const MENTION_COUNT_OUT_OF_RANGE_ERROR: &str =
    "mentions#count exceeds the maximum supported value.";
const MENTION_COUNT_INVALID_ERROR: &str = "mentions#count must be a non-negative integer.";
const MEMBERS_ONLY_INVALID_ERROR: &str = "muc#roomconfig_membersonly must be a boolean.";
const MODERATED_ROOM_INVALID_ERROR: &str = "muc#roomconfig_moderatedroom must be a boolean.";
const ENABLE_LOGGING_INVALID_ERROR: &str = "muc#roomconfig_enablelogging must be a boolean.";
const FORUM_INVALID_ERROR: &str = "muc#roomconfig_forum must be a boolean.";
const MENTION_INDIVIDUAL_INVALID_ERROR: &str =
    "mentions#individual must be participants, moderators, or none.";
const MENTION_CHANNEL_INVALID_ERROR: &str =
    "mentions#channel must be participants, moderators, or none.";
const PIN_PERMISSION_INVALID_ERROR: &str =
    "urn:waddle:roomconfig:pinpermission must be admins-only or anyone.";
const FIELD_VALUE_COUNT_ERROR: &str = "owner-config fields must contain exactly one value.";
const FIELD_DUPLICATE_ERROR: &str = "owner-config fields must not be duplicated.";

#[derive(Debug, thiserror::Error)]
pub(super) enum MucOwnerConfigError {
    #[error("{0}")]
    BadRequest(&'static str),
    #[error("{0}")]
    Internal(String),
}

fn data_form_single_value(
    form: &Element,
    var: &str,
) -> Result<Option<String>, MucOwnerConfigError> {
    let fields = form
        .children()
        .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
        .filter(|field| field.attr("var") == Some(var))
        .collect::<Vec<_>>();
    let field = match fields.as_slice() {
        [] => {
            return Ok(None);
        }
        [field] => *field,
        _ => {
            return Err(MucOwnerConfigError::BadRequest(FIELD_DUPLICATE_ERROR));
        }
    };
    let values = field
        .children()
        .filter(|child| child.name() == "value" && child.ns() == DATA_FORMS_NS)
        .collect::<Vec<_>>();
    match values.as_slice() {
        [value] => Ok(Some(value.texts().collect())),
        _ => Err(MucOwnerConfigError::BadRequest(FIELD_VALUE_COUNT_ERROR)),
    }
}

fn data_form_bool(
    form: &Element,
    var: &str,
    invalid_error: &'static str,
) -> Result<Option<bool>, MucOwnerConfigError> {
    let Some(value) = data_form_single_value(form, var)? else {
        return Ok(None);
    };
    match value.as_str() {
        "1" | "true" | "True" | "TRUE" => Ok(Some(true)),
        "0" | "false" | "False" | "FALSE" => Ok(Some(false)),
        _ => Err(MucOwnerConfigError::BadRequest(invalid_error)),
    }
}

fn data_form_mention_count(form: &Element) -> Result<Option<u32>, MucOwnerConfigError> {
    let Some(value) = data_form_single_value(form, waddle_xmpp::xep::FIELD_MENTIONS_COUNT)? else {
        return Ok(None);
    };
    let count = match value.parse::<u64>() {
        Ok(count) => count,
        Err(_) if !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()) => {
            return Err(MucOwnerConfigError::BadRequest(
                MENTION_COUNT_OUT_OF_RANGE_ERROR,
            ));
        }
        Err(_) => return Err(MucOwnerConfigError::BadRequest(MENTION_COUNT_INVALID_ERROR)),
    };
    if count > u64::from(MAX_MENTION_PERMISSIONS_COUNT) {
        return Err(MucOwnerConfigError::BadRequest(
            MENTION_COUNT_OUT_OF_RANGE_ERROR,
        ));
    }
    Ok(Some(count as u32))
}

fn data_form_mention_permission(
    form: &Element,
    var: &str,
    invalid_error: &'static str,
) -> Result<Option<waddle_xmpp::xep::MentionPermission>, MucOwnerConfigError> {
    let Some(value) = data_form_single_value(form, var)? else {
        return Ok(None);
    };
    waddle_xmpp::xep::MentionPermission::from_form_value(&value)
        .map(Some)
        .ok_or(MucOwnerConfigError::BadRequest(invalid_error))
}

fn data_form_pin_permission(
    form: &Element,
) -> Result<Option<waddle_xmpp::muc::PinPermission>, MucOwnerConfigError> {
    let Some(value) = data_form_single_value(form, waddle_xmpp::muc::owner::FIELD_PIN_PERMISSION)?
    else {
        return Ok(None);
    };
    waddle_xmpp::muc::PinPermission::from_form_value(&value)
        .map(Some)
        .ok_or(MucOwnerConfigError::BadRequest(
            PIN_PERMISSION_INVALID_ERROR,
        ))
}

pub(super) async fn apply_muc_owner_config(
    state: &WebSocketState,
    room_jid: &BareJid,
    iq: &xmpp_parsers::iq::Iq,
    session: Option<&Session>,
) -> Result<(), MucOwnerConfigError> {
    let room_actor = get_room_actor(state, room_jid)
        .await
        .ok_or_else(|| MucOwnerConfigError::Internal("room actor not found".to_string()))?;
    let mut config = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| MucOwnerConfigError::Internal(format!("snapshot failed: {error:?}")))?
        .room
        .config;

    if let xmpp_parsers::iq::IqType::Set(query) = &iq.payload {
        if let Some(form) = query.get_child("x", DATA_FORMS_NS) {
            if let Some(name) = data_form_single_value(form, "muc#roomconfig_roomname")?
                .filter(|value| !value.is_empty())
            {
                config.name = name;
            }
            // Treat presence of the roomdesc field as authoritative
            // even when empty, so an owner can clear the description.
            // Field absent => keep existing; field present + empty
            // => clear; field present + non-empty => set.
            if let Some(value) = data_form_single_value(form, "muc#roomconfig_roomdesc")? {
                config.description = if value.is_empty() { None } else { Some(value) };
            }
            if let Some(members_only) = data_form_bool(
                form,
                "muc#roomconfig_membersonly",
                MEMBERS_ONLY_INVALID_ERROR,
            )? {
                config.members_only = members_only;
            }
            if let Some(moderated) = data_form_bool(
                form,
                "muc#roomconfig_moderatedroom",
                MODERATED_ROOM_INVALID_ERROR,
            )? {
                config.moderated = moderated;
            }
            if let Some(enable_logging) = data_form_bool(
                form,
                "muc#roomconfig_enablelogging",
                ENABLE_LOGGING_INVALID_ERROR,
            )? {
                config.enable_logging = enable_logging;
            }
            if let Some(forum) = data_form_bool(form, "muc#roomconfig_forum", FORUM_INVALID_ERROR)?
            {
                config.forum = forum;
            }
            if let Some(count) = data_form_mention_count(form)? {
                config.mention_permissions.count = count;
            }
            if let Some(permission) = data_form_mention_permission(
                form,
                waddle_xmpp::xep::FIELD_MENTIONS_INDIVIDUAL,
                MENTION_INDIVIDUAL_INVALID_ERROR,
            )? {
                config.mention_permissions.individual = permission;
            }
            if let Some(permission) = data_form_mention_permission(
                form,
                waddle_xmpp::xep::FIELD_MENTIONS_CHANNEL,
                MENTION_CHANNEL_INVALID_ERROR,
            )? {
                config.mention_permissions.channel = permission;
            }
            // #415: per-room pin permission policy.
            if let Some(pin_permission) = data_form_pin_permission(form)? {
                config.pin_permission = pin_permission;
            }
        }
    }

    // Waddle rooms are persistent, non-anonymous collaboration surfaces.
    config.persistent = true;

    if let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) {
        let now = chrono::Utc::now().to_rfc3339();
        let actor = state.deps.app_state.db_pool.global_actor().clone();
        actor
            .ask(DbExecute {
                sql: r#"
                INSERT INTO channels (
                    id, name, description, channel_type, position, is_default,
                    pin_permission,
                    mention_permissions_count,
                    mention_permissions_individual,
                    mention_permissions_channel,
                    created_at, updated_at
                )
                VALUES (?, ?, ?, ?, 0, 0, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    description = excluded.description,
                    channel_type = excluded.channel_type,
                    pin_permission = excluded.pin_permission,
                    mention_permissions_count = excluded.mention_permissions_count,
                    mention_permissions_individual = excluded.mention_permissions_individual,
                    mention_permissions_channel = excluded.mention_permissions_channel,
                    updated_at = excluded.updated_at
            "#
                .to_string(),
                params: vec![
                    channel_id.clone().into(),
                    config.name.clone().into(),
                    config.description.clone().into(),
                    (if config.forum { "forum" } else { "text" }).into(),
                    config.pin_permission.as_form_value().into(),
                    config.mention_permissions.count.into(),
                    config.mention_permissions.individual.as_form_value().into(),
                    config.mention_permissions.channel.as_form_value().into(),
                    now.clone().into(),
                    now.into(),
                ],
            })
            .await
            .map_err(|error| {
                MucOwnerConfigError::Internal(format!("channel upsert failed: {error}"))
            })?;

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
                .map_err(|error| {
                    MucOwnerConfigError::Internal(format!("channel owner tuple failed: {error}"))
                })?;
            }
            None => {
                warn!(
                    channel_id = %channel_id,
                    "apply_muc_owner_config called without a session; \
                 channel owner tuple not written — room may be inaccessible after server restart"
                );
            }
        }
    }

    room_actor
        .ask(UpdateConfig { config })
        .await
        .map_err(|error| {
            MucOwnerConfigError::Internal(format!("config update failed: {error:?}"))
        })?;

    Ok(())
}
