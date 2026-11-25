//! Create an event workspace only after the room confirms privacy and persistence.
use jid::BareJid;
use minidom::Element;
use std::future::Future;
use uuid::Uuid;
use waddle_xmpp_client::discovery::{build_disco_info_iq, parse_disco_info_result};
use waddle_xmpp_client::xep::xep0045_owner::{
    build_muc_owner_config_get_iq, build_muc_owner_config_submit_iq, parse_muc_owner_config_form,
    FIELD_MEMBERS_ONLY, FIELD_PUBLIC_ROOM, FIELD_ROOM_NAME,
};
use waddle_xmpp_client::xep::xep0402::{build_publish_bookmark_iq, BookmarkItem};

const PERSISTENCE_FIELD: &str = "muc#roomconfig_persistentroom";

/// Both adapters use the same GET, submit, verify, discover and bookmark sequence.
pub async fn configure<E, F>(
    room: &BareJid,
    name: &str,
    nick: String,
    private: bool,
    mut send: impl FnMut(Element) -> F,
    invalid: impl Fn(&'static str) -> E,
) -> Result<(), E>
where
    F: Future<Output = Result<Element, E>>,
{
    let reply = send(build_muc_owner_config_get_iq(room)).await?;
    let mut form = parse_muc_owner_config_form(&reply)
        .ok_or_else(|| invalid("room configuration unavailable"))?;
    if form.value(FIELD_MEMBERS_ONLY).is_none() || form.value(FIELD_PUBLIC_ROOM).is_none() {
        return Err(invalid("room access controls unavailable"));
    }
    form.set_value(
        FIELD_MEMBERS_ONLY,
        if private { "1" } else { "0" }.to_owned(),
    );
    form.set_value(
        FIELD_PUBLIC_ROOM,
        if private { "0" } else { "1" }.to_owned(),
    );
    if form
        .fields
        .iter()
        .any(|field| field.var == PERSISTENCE_FIELD)
    {
        form.set_value(PERSISTENCE_FIELD, "1".into());
    }
    send(build_muc_owner_config_submit_iq(room, &form)).await?;
    let verified = send(build_muc_owner_config_get_iq(room)).await?;
    let mut verified = parse_muc_owner_config_form(&verified)
        .ok_or_else(|| invalid("room privacy unavailable"))?;
    if verified.bool_value(FIELD_MEMBERS_ONLY) != Some(private)
        || verified.bool_value(FIELD_PUBLIC_ROOM) != Some(!private)
    {
        return Err(invalid(
            "the service did not apply the requested room privacy",
        ));
    }
    let info = send(build_disco_info_iq(room.as_str(), None)).await?;
    if !parse_disco_info_result(&info, room.as_str())
        .is_some_and(|info| info.has_feature("muc_persistent"))
    {
        return Err(invalid("event rooms must be persistent"));
    }
    verified.set_value(FIELD_ROOM_NAME, name.to_owned());
    send(build_muc_owner_config_submit_iq(room, &verified)).await?;
    let bookmark = BookmarkItem {
        jid: room.clone(),
        name: Some(name.to_owned()),
        autojoin: true,
        nick: Some(nick),
        password: None,
        extensions: Vec::new(),
    };
    send(build_publish_bookmark_iq(
        &bookmark,
        &Uuid::new_v4().to_string(),
    ))
    .await?;
    Ok(())
}
