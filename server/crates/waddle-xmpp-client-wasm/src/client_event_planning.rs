use super::{jid_domain, send_stanza_command};
use super::{js_error, send_iq_command, service_bare_jid, to_js_value, WaddleClient};
use jid::BareJid;
use uuid::Uuid;
use waddle_xmpp_client::discovery::{
    build_disco_info_iq, build_disco_items_iq, parse_disco_info_result, parse_disco_items_result,
};
use waddle_xmpp_client::messaging::build_muc_join_presence;
use waddle_xmpp_client::xep::xep0045_owner::build_muc_owner_destroy_iq;
use waddle_xmpp_client_community::planning;
use wasm_bindgen::prelude::{wasm_bindgen, JsValue};
use wasm_bindgen_futures::future_to_promise;

#[wasm_bindgen]
impl WaddleClient {
    /// Read the authenticated member's planning objects for this room.
    pub fn event_planning_items(&self, room_jid: String) -> js_sys::Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let room = service_bare_jid(&room_jid)?;
            let reply = send_iq_command(inner, planning::items(&room, None)).await?;
            to_js_value(&planning::parse_items(&reply, &room).map_err(js_error)?)
        })
    }

    /// Publish one object and read back the canonical version from the room.
    pub fn event_planning_publish(&self, room_jid: String, input: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let room = service_bare_jid(&room_jid)?;
            let owner = inner
                .borrow()
                .config
                .jid
                .parse::<jid::Jid>()
                .map_err(|_| js_error("invalid account JID"))?
                .to_bare();
            let value = serde_wasm_bindgen::from_value(input)
                .map_err(|_| js_error("invalid planning item"))?;
            let (id, iq) = planning::publish(&room, owner, value).map_err(js_error)?;
            send_iq_command(inner.clone(), iq).await?;
            let reply = send_iq_command(inner, planning::items(&room, Some(&id))).await?;
            to_js_value(&planning::published(&reply, &room, &id).map_err(js_error)?)
        })
    }

    /// Remove a planning object using its pubsub item ID.
    pub fn event_planning_retract(&self, room_jid: String, item_id: String) -> js_sys::Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let room = service_bare_jid(&room_jid)?;
            send_iq_command(inner, planning::retract(&room, &item_id).map_err(js_error)?).await?;
            Ok(JsValue::UNDEFINED)
        })
    }
    /// Create and bookmark an event conversation. Private workspaces require
    /// members-only and hidden-room fields to be supported by the service.
    /// Configure access before publishing any planning metadata.
    pub fn event_room_create(&self, name: String, private_room: bool) -> js_sys::Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
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

            let setup = waddle_xmpp_client_community::plan_workspace::configure(
                &room,
                name,
                nick,
                private_room,
                |request| send_iq_command(inner.clone(), request),
                js_error,
            )
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
