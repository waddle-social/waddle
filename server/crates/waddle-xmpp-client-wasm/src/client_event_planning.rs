//! Room-scoped event workspaces. XEP-0045 controls membership; XEP-0060
//! persists independent planning items behind the same membership gate.

use jid::BareJid;
use uuid::Uuid;
use waddle_xmpp_client::discovery::{
    build_disco_info_iq, build_disco_items_iq, parse_disco_info_result, parse_disco_items_result,
};
use waddle_xmpp_client::messaging::build_muc_join_presence;
use waddle_xmpp_client::pubsub::{
    build_pubsub_items_iq, build_pubsub_publish_iq, build_pubsub_retract_iq,
    parse_pubsub_items_result,
};
use waddle_xmpp_client::xep::xep0045_owner::{
    build_muc_owner_config_get_iq, build_muc_owner_config_submit_iq, build_muc_owner_destroy_iq,
    parse_muc_owner_config_form, FEATURE_MUC_PERSISTENT, FIELD_MEMBERS_ONLY, FIELD_PERSISTENT_ROOM,
    FIELD_PUBLIC_ROOM, FIELD_ROOM_NAME,
};
use waddle_xmpp_client::xep::xep0402::{build_publish_bookmark_iq, BookmarkItem};
use waddle_xmpp_core::event_planning::{PlanningItem, NODE_EVENT_PLANNING};
use wasm_bindgen::prelude::{wasm_bindgen, JsValue};

use super::{
    future_to_promise, jid_domain, js_error, send_iq_command, send_stanza_command, to_js_value,
    WaddleClient,
};

mod wire;
use wire::JsPlanningItem;

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch objects, checking room membership on every request.
    pub fn event_planning_items(&self, room_jid: String) -> js_sys::Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let room: BareJid = room_jid
                .parse()
                .map_err(|_| js_error("invalid event room JID"))?;
            let iq = build_pubsub_items_iq(
                &Uuid::new_v4().to_string(),
                Some(&room),
                NODE_EVENT_PLANNING,
                None,
            );
            let result = send_iq_command(inner, iq).await?;
            let mut items = Vec::new();
            for item in parse_pubsub_items_result(&result) {
                let payload = item
                    .payload("item", NODE_EVENT_PLANNING)
                    .ok_or_else(|| js_error("invalid planning reply"))?;
                let parsed = PlanningItem::from_element(payload)
                    .map_err(|error| js_error(error.to_string()))?;
                items.push(JsPlanningItem::from(parsed));
            }
            to_js_value(&items)
        })
    }

    /// Publish one object with the authenticated author's identity.
    pub fn event_planning_publish(&self, room_jid: String, input: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let room: BareJid = room_jid
                .parse()
                .map_err(|_| js_error("invalid event room JID"))?;
            let input: JsPlanningItem = serde_wasm_bindgen::from_value(input)
                .map_err(|error| js_error(error.to_string()))?;
            let mut item = input.into_core().map_err(js_error)?;
            let account: BareJid = inner
                .borrow()
                .config
                .jid
                .split('/')
                .next()
                .unwrap_or_default()
                .parse()
                .map_err(|_| js_error("invalid account JID"))?;
            item.author = Some(account);
            item.validate()
                .map_err(|error| js_error(error.to_string()))?;
            let iq = build_pubsub_publish_iq(
                &Uuid::new_v4().to_string(),
                Some(&room),
                NODE_EVENT_PLANNING,
                Some(&item.id),
                item.to_element(),
                None,
            );
            send_iq_command(inner.clone(), iq).await?;
            let mut query = build_pubsub_items_iq(
                &Uuid::new_v4().to_string(),
                Some(&room),
                NODE_EVENT_PLANNING,
                None,
            );
            if let Some(items) = query
                .get_child_mut("pubsub", waddle_xmpp_client::pep::NS_PUBSUB)
                .and_then(|pubsub| {
                    pubsub.get_child_mut("items", waddle_xmpp_client::pep::NS_PUBSUB)
                })
            {
                items.append_child(
                    minidom::Element::builder("item", waddle_xmpp_client::pep::NS_PUBSUB)
                        .attr(
                            minidom::rxml::xml_ncname!("id").to_owned(),
                            item.id.as_str(),
                        )
                        .build(),
                );
            }
            let result = send_iq_command(inner, query).await?;
            let stored = parse_pubsub_items_result(&result);
            let payload = stored
                .iter()
                .find(|stored| stored.id == item.id)
                .and_then(|stored| stored.payload("item", NODE_EVENT_PLANNING))
                .ok_or_else(|| js_error("saved planning object unavailable"))?;
            let canonical =
                PlanningItem::from_element(payload).map_err(|error| js_error(error.to_string()))?;
            to_js_value(&JsPlanningItem::from(canonical))
        })
    }

    /// Remove one object, enforcing author and organiser permissions on the service.
    pub fn event_planning_retract(&self, room_jid: String, item_id: String) -> js_sys::Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let room: BareJid = room_jid
                .parse()
                .map_err(|_| js_error("invalid event room JID"))?;
            if item_id.is_empty() {
                return Err(js_error("item identifier is required"));
            }
            let iq = build_pubsub_retract_iq(
                &Uuid::new_v4().to_string(),
                Some(&room),
                NODE_EVENT_PLANNING,
                &item_id,
            );
            send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Create and bookmark an event conversation. Private workspaces require
    /// members-only and hidden-room fields to be supported by the service.
    /// Configure access before publishing any planning metadata.
    pub fn event_room_create(&self, name: String, private_room: bool) -> js_sys::Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            if !private_room {
                return Err(js_error("event workspaces must be private"));
            }
            let name = name.trim();
            if name.is_empty() || name.len() > 200 {
                return Err(js_error("event title must contain 1–200 characters"));
            }
            let account: BareJid = inner
                .borrow()
                .config
                .jid
                .split('/')
                .next()
                .unwrap_or_default()
                .parse()
                .map_err(|_| js_error("invalid account JID"))?;
            let nick = account
                .node()
                .ok_or_else(|| js_error("account has no username"))?
                .to_string();
            let items = send_iq_command(
                inner.clone(),
                build_disco_items_iq(&jid_domain(account.as_str()), None),
            )
            .await?;
            let services = parse_disco_items_result(&items)
                .ok_or_else(|| js_error("invalid service discovery reply"))?;
            let mut muc = None;
            for service in services {
                let result =
                    send_iq_command(inner.clone(), build_disco_info_iq(&service.jid, None)).await?;
                if parse_disco_info_result(&result, &service.jid).is_some_and(|info| {
                    info.identities.iter().any(|identity| {
                        identity.category == "conference" && identity.identity_type == "text"
                    })
                }) {
                    muc = Some(
                        service
                            .jid
                            .parse::<BareJid>()
                            .map_err(|_| js_error("invalid room service JID"))?,
                    );
                    break;
                }
            }
            let muc = muc.ok_or_else(|| js_error("no conversation service available"))?;
            let localpart = format!("event-{}", Uuid::new_v4());
            let node =
                jid::NodePart::new(&localpart).map_err(|_| js_error("invalid event room name"))?;
            let room = BareJid::from_parts(Some(&node), muc.domain());
            let presence = build_muc_join_presence(&room, &nick)
                .map_err(|error| js_error(error.to_string()))?;
            send_stanza_command(inner.clone(), presence).await?;
            let setup = async {
                let result =
                    send_iq_command(inner.clone(), build_muc_owner_config_get_iq(&room)).await?;
                let mut form = parse_muc_owner_config_form(&result)
                    .ok_or_else(|| js_error("room configuration unavailable"))?;
                if private_room
                    && (form.value(FIELD_MEMBERS_ONLY).is_none()
                        || form.value(FIELD_PUBLIC_ROOM).is_none())
                {
                    return Err(js_error(
                        "this service cannot create a private event workspace",
                    ));
                }
                form.set_value(FIELD_ROOM_NAME, name.to_string());
                if form.value(FIELD_PERSISTENT_ROOM).is_some() {
                    form.set_value(FIELD_PERSISTENT_ROOM, "1".to_string());
                }
                form.set_value(
                    FIELD_MEMBERS_ONLY,
                    if private_room { "1" } else { "0" }.to_string(),
                );
                form.set_value(
                    FIELD_PUBLIC_ROOM,
                    if private_room { "0" } else { "1" }.to_string(),
                );
                send_iq_command(
                    inner.clone(),
                    build_muc_owner_config_submit_iq(&room, &form),
                )
                .await?;
                let configured =
                    send_iq_command(inner.clone(), build_muc_owner_config_get_iq(&room)).await?;
                let configured = parse_muc_owner_config_form(&configured)
                    .ok_or_else(|| js_error("cannot verify event room privacy"))?;
                if configured.bool_value(FIELD_MEMBERS_ONLY) != Some(private_room)
                    || configured.bool_value(FIELD_PUBLIC_ROOM) != Some(!private_room)
                {
                    return Err(js_error(
                        "service did not apply event privacy and persistence settings",
                    ));
                }
                let info = send_iq_command(inner.clone(), build_disco_info_iq(room.as_str(), None))
                    .await?;
                if !parse_disco_info_result(&info, room.as_str())
                    .is_some_and(|info| info.has_feature(FEATURE_MUC_PERSISTENT))
                {
                    return Err(js_error("event workspace must be persistent"));
                }
                let bookmark = BookmarkItem {
                    jid: room.clone(),
                    name: Some(name.to_string()),
                    autojoin: true,
                    nick: Some(nick),
                    password: None,
                    extensions: Vec::new(),
                };
                send_iq_command(
                    inner.clone(),
                    build_publish_bookmark_iq(&bookmark, &Uuid::new_v4().to_string()),
                )
                .await?;
                Ok::<(), JsValue>(())
            }
            .await;
            if let Err(error) = setup {
                let _ = send_iq_command(
                    inner,
                    build_muc_owner_destroy_iq(&room, Some("event setup failed")),
                )
                .await;
                return Err(error);
            }
            Ok(JsValue::from_str(room.as_str()))
        })
    }
}
