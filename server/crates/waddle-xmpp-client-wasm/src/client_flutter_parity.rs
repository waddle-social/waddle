//! Typed operations needed by the standalone Flutter browser client.
//!
//! These adapt existing shared XMPP builders and parsers, without a raw-XML
//! escape hatch or a second implementation of protocol serialization.

use std::cell::RefCell;
use std::rc::Rc;

use jid::{BareJid, NodePart};
use js_sys::Promise;
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_client::avatar::{
    build_disable_avatar_iq, build_publish_avatar_data_iq, build_publish_avatar_metadata_iq,
    compute_avatar_item_id, AvatarPublishInfo,
};
use waddle_xmpp_client::discovery::{
    build_muc_admin_affiliation_set_iq, build_muc_admin_role_set_iq, MucAdminAffiliationItem,
};
use waddle_xmpp_client::error::STANZA_CONDITION_ITEM_NOT_FOUND;
use waddle_xmpp_client::messaging::{
    build_finish_with_reason, build_link_preview_lookup_iq, build_muc_join_presence,
    eligible_preview_url, jingle_reason_from_wire_name, parse_link_preview_lookup_response,
    wrap_jmi_message, LinkPreviewLookup, MucAffiliation, MucRole, SessionId,
};
use waddle_xmpp_client::stickers::{
    build_pack_publish_iq, build_pack_retract_iq, build_sticker_pack_items_iq,
    build_sticker_packs_items_iq, parse_sticker_packs_result, PackId, STICKERS_NODE,
};
use waddle_xmpp_client::xep::xep0045_owner::{
    apply_room_config_patch, build_muc_owner_config_get_iq, build_muc_owner_config_submit_iq,
    build_muc_owner_destroy_iq, parse_muc_owner_config_form, RoomConfigForm, RoomPinPermission,
    FIELD_FORUM_MODE, FIELD_MEMBERS_ONLY, FIELD_MODERATED_ROOM, FIELD_PIN_PERMISSION,
    FIELD_PUBLIC_ROOM, FIELD_ROOM_DESC, FIELD_ROOM_NAME,
};
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::future_to_promise;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::ping::Ping;
use xmpp_parsers::presence::Presence;

use crate::client_flutter_account_parity::{discover_services, execute_account, AccountCommand};
use crate::commands::{
    send_iq_command, send_iq_command_stanza_aware, send_ping_command, send_stanza_command,
};
use crate::flutter_parity_types::{
    pack_from_boundary, pack_to_boundary, Config, ConfigPatch, ParityCommand, PinPermission,
    StickerPack,
};
use crate::helpers::{js_error, to_js_value};
use crate::{WaddleClient, WaddleClientInner, WaddleLinkPreviewImage, WaddleLinkPreviewPlayer};

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FlutterCommand {
    Parity(ParityCommand),
    Account(AccountCommand),
}

#[wasm_bindgen]
impl WaddleClient {
    /// Execute a typed native-compatible Flutter operation absent from the older
    /// browser facade. Malformed requests and protocol failures reject the Promise.
    pub fn flutter_rpc(&self, request: JsValue) -> Promise {
        let command = serde_wasm_bindgen::from_value::<FlutterCommand>(request);
        let inner = self.inner.clone();
        future_to_promise(async move {
            match command.map_err(|error| js_error(error.to_string()))? {
                FlutterCommand::Parity(command) => execute(inner, command).await,
                FlutterCommand::Account(command) => execute_account(inner, command).await,
            }
        })
    }
}

async fn execute(
    inner: Rc<RefCell<WaddleClientInner>>,
    command: ParityCommand,
) -> Result<JsValue, JsValue> {
    match command {
        ParityCommand::CreateRoom {
            localpart,
            nick,
            patch,
        } => {
            let node =
                NodePart::new(localpart.trim()).map_err(|error| js_error(error.to_string()))?;
            let service = discover_services(inner.clone()).await?;
            let room = BareJid::from_parts(Some(&node), service.muc.domain());
            let presence = build_muc_join_presence(&room, &nick)
                .map_err(|error| js_error(error.to_string()))?;
            send_stanza_command(inner.clone(), presence).await?;
            let configure = configure(inner.clone(), &room, patch).await;
            if let Ok(occupant) = room.with_resource_str(&nick) {
                let _ =
                    send_stanza_command(inner, Presence::unavailable().with_to(occupant).into())
                        .await;
            }
            configure?;
            Ok(JsValue::from_str(room.as_str()))
        }
        ParityCommand::FetchRoomConfig { room_jid } => {
            let form = fetch_form(inner, &room_jid).await?;
            to_js_value(&config(&form))
        }
        ParityCommand::SubmitRoomConfig { room_jid, patch } => {
            configure(inner, &room_jid, patch).await?;
            Ok(JsValue::UNDEFINED)
        }
        ParityCommand::DestroyRoom { room_jid, reason } => {
            send_iq_command(
                inner,
                build_muc_owner_destroy_iq(&room_jid, reason.as_deref()),
            )
            .await?;
            Ok(JsValue::UNDEFINED)
        }
        ParityCommand::KickOccupant {
            room_jid,
            nick,
            reason,
        } => {
            send_iq_command(
                inner,
                build_muc_admin_role_set_iq(
                    room_jid.as_str(),
                    &nick,
                    MucRole::None,
                    reason.as_deref(),
                ),
            )
            .await?;
            Ok(JsValue::UNDEFINED)
        }
        ParityCommand::SetRoomAffiliation {
            room_jid,
            target_jid,
            affiliation,
            reason,
        } => {
            send_iq_command(
                inner,
                affiliation_request(&room_jid, &target_jid, affiliation, reason),
            )
            .await?;
            Ok(JsValue::UNDEFINED)
        }
        ParityCommand::PublishAvatar {
            data,
            mime_type,
            width,
            height,
        } => {
            let (id, data_iq, metadata_iq) =
                avatar_requests(&data, &mime_type, width, height).map_err(js_error)?;
            // XEP-0084 requires the data ACK before announcing that item's metadata.
            send_iq_command(inner.clone(), data_iq).await?;
            send_iq_command(inner, metadata_iq).await?;
            Ok(JsValue::from_str(&id))
        }
        ParityCommand::DisableAvatar {} => {
            send_iq_command(inner, build_disable_avatar_iq()).await?;
            Ok(JsValue::UNDEFINED)
        }
        ParityCommand::FetchStickerPacks { owner_jid } => {
            let iq = build_sticker_packs_items_iq(&Uuid::new_v4().to_string(), owner_jid.as_ref());
            let result = send_iq_command_stanza_aware(inner, iq).await?;
            match result {
                Ok(result) => to_js_value(
                    &parse_sticker_packs_result(&result, STICKERS_NODE)
                        .into_iter()
                        .map(pack_to_boundary)
                        .collect::<Vec<_>>(),
                ),
                Err(error) if error.condition == STANZA_CONDITION_ITEM_NOT_FOUND => {
                    to_js_value(&Vec::<StickerPack>::new())
                }
                Err(error) => Err(js_error(error.to_string())),
            }
        }
        ParityCommand::FetchStickerPack {
            owner_jid,
            node,
            pack_id,
        } => {
            let id = PackId::try_new(pack_id)
                .ok_or_else(|| js_error("sticker pack ID must not be empty"))?;
            let node = node
                .filter(|node| !node.trim().is_empty())
                .unwrap_or_else(|| STICKERS_NODE.to_string());
            let iq =
                build_sticker_pack_items_iq(&Uuid::new_v4().to_string(), &owner_jid, &node, &id);
            match send_iq_command_stanza_aware(inner, iq).await? {
                Ok(result) => to_js_value(
                    &parse_sticker_packs_result(&result, &node)
                        .into_iter()
                        .find(|pack| pack.id == id)
                        .map(pack_to_boundary),
                ),
                Err(error) if error.condition == STANZA_CONDITION_ITEM_NOT_FOUND => {
                    Ok(JsValue::NULL)
                }
                Err(error) => Err(js_error(error.to_string())),
            }
        }
        ParityCommand::PublishStickerPack { pack } => {
            let doc = pack_from_boundary(pack).map_err(js_error)?;
            let (id, iq) = build_pack_publish_iq(&Uuid::new_v4().to_string(), &doc);
            send_iq_command(inner, iq).await?;
            Ok(JsValue::from_str(id.as_str()))
        }
        ParityCommand::RetractStickerPack { pack_id } => {
            let id = PackId::try_new(pack_id)
                .ok_or_else(|| js_error("sticker pack ID must not be empty"))?;
            let iq = build_pack_retract_iq(&Uuid::new_v4().to_string(), &id);
            match send_iq_command_stanza_aware(inner, iq).await? {
                Ok(_) => Ok(JsValue::TRUE),
                Err(error) if error.condition == STANZA_CONDITION_ITEM_NOT_FOUND => {
                    Ok(JsValue::TRUE)
                }
                Err(error) => Err(js_error(error.to_string())),
            }
        }
        ParityCommand::LookupLinkPreview { url, scope_jid } => {
            let Some(url) = eligible_preview_url(&url) else {
                return to_js_value(&Lookup {
                    status: "unsupported",
                    preview: None,
                });
            };
            let iq = build_link_preview_lookup_iq(&url, &scope_jid);
            let result = send_iq_command(inner, iq).await?;
            to_js_value(&lookup(parse_link_preview_lookup_response(&result, &url)))
        }
        ParityCommand::PingServer {} => {
            let iq = Iq::from_get(Uuid::new_v4().to_string(), Ping).into();
            // A stanza error is still proof of a responsive authenticated stream.
            send_ping_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        }
        ParityCommand::SendCallFinishWithReason {
            peer_full_jid,
            sid,
            reason,
        } => {
            if sid.is_empty() {
                return Err(js_error("call session ID must not be empty"));
            }
            let reason = jingle_reason_from_wire_name(reason.wire())
                .ok_or_else(|| js_error("unsupported Jingle reason"))?;
            let stanza = wrap_jmi_message(
                &peer_full_jid.into(),
                build_finish_with_reason(&SessionId(sid), Some(reason)),
            );
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::TRUE)
        }
    }
}

fn affiliation_request(
    room: &BareJid,
    target: &BareJid,
    affiliation: MucAffiliation,
    reason: Option<String>,
) -> Element {
    let item = MucAdminAffiliationItem {
        jid: Some(target.to_string()),
        nick: None,
        affiliation: Some(affiliation),
        reason: reason.filter(|reason| !reason.is_empty()),
    };
    build_muc_admin_affiliation_set_iq(room.as_str(), &[item])
}

async fn fetch_form(
    inner: Rc<RefCell<WaddleClientInner>>,
    room: &BareJid,
) -> Result<RoomConfigForm, JsValue> {
    let result = send_iq_command(inner, build_muc_owner_config_get_iq(room)).await?;
    parse_muc_owner_config_form(&result)
        .ok_or_else(|| js_error("malformed room configuration form"))
}

async fn configure(
    inner: Rc<RefCell<WaddleClientInner>>,
    room: &BareJid,
    patch: ConfigPatch,
) -> Result<(), JsValue> {
    let mut form = fetch_form(inner.clone(), room).await?;
    apply_room_config_patch(&mut form, &patch.into());
    send_iq_command(inner, build_muc_owner_config_submit_iq(room, &form)).await?;
    Ok(())
}

fn config(form: &RoomConfigForm) -> Config {
    Config {
        name: form.value(FIELD_ROOM_NAME).map(str::to_string),
        description: form.value(FIELD_ROOM_DESC).map(str::to_string),
        members_only: form.bool_value(FIELD_MEMBERS_ONLY),
        public_room: form.bool_value(FIELD_PUBLIC_ROOM),
        moderated: form.bool_value(FIELD_MODERATED_ROOM),
        forum: form.bool_value(FIELD_FORUM_MODE),
        pin_permission: form
            .value(FIELD_PIN_PERMISSION)
            .and_then(RoomPinPermission::from_form_value)
            .and_then(|value| match value {
                RoomPinPermission::AdminsOnly => Some(PinPermission::AdminsOnly),
                RoomPinPermission::Anyone => Some(PinPermission::Anyone),
                _ => None,
            }),
    }
}

fn avatar_requests(
    data: &[u8],
    mime_type: &str,
    width: u32,
    height: u32,
) -> Result<(String, Element, Element), &'static str> {
    if data.is_empty()
        || mime_type != "image/png"
        || width > u32::from(u16::MAX)
        || height > u32::from(u16::MAX)
    {
        return Err("avatar must be nonempty PNG with valid dimensions");
    }
    let bytes = u32::try_from(data.len()).map_err(|_| "avatar is too large")?;
    let id = compute_avatar_item_id(data);
    let data_iq = build_publish_avatar_data_iq(&id, data);
    let metadata_iq = build_publish_avatar_metadata_iq(&AvatarPublishInfo {
        bytes,
        id: id.clone(),
        mime_type: mime_type.to_string(),
        width: (width > 0).then_some(width),
        height: (height > 0).then_some(height),
    });
    Ok((id.to_string(), data_iq, metadata_iq))
}

#[derive(Serialize)]
struct Lookup {
    status: &'static str,
    preview: Option<Preview>,
}

#[derive(Serialize)]
struct Preview {
    token: String,
    original_url: String,
    normalized_url: String,
    expires_at: String,
    title: Option<String>,
    description: Option<String>,
    image: Option<WaddleLinkPreviewImage>,
    player_embed: Option<WaddleLinkPreviewPlayer>,
}

fn lookup(value: LinkPreviewLookup) -> Lookup {
    match value {
        LinkPreviewLookup::Ready(ready) => Lookup {
            status: "ready",
            preview: Some(Preview {
                token: ready.token.as_str().to_string(),
                original_url: ready.original_url.to_string(),
                normalized_url: ready.normalized_url.to_string(),
                expires_at: ready.expires_at.to_rfc3339(),
                title: ready.title,
                description: ready.description,
                image: ready.image.map(|image| WaddleLinkPreviewImage {
                    url: image.url.to_string(),
                    media_type: image.media_type.as_str().to_string(),
                    width: image.width,
                    height: image.height,
                    alt: image.alt,
                }),
                player_embed: ready.player_embed.map(|player| WaddleLinkPreviewPlayer {
                    url: player.url.to_string(),
                    width: player.width,
                    height: player.height,
                }),
            }),
        },
        LinkPreviewLookup::Unsupported => Lookup {
            status: "unsupported",
            preview: None,
        },
        LinkPreviewLookup::Blocked => Lookup {
            status: "blocked",
            preview: None,
        },
        _ => Lookup {
            status: "failed",
            preview: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use waddle_xmpp_client::avatar::{NS_AVATAR_DATA, NS_AVATAR_METADATA};
    use waddle_xmpp_client::stickers::build_pack_element;

    #[test]
    fn xep0045_affiliation_boundary_preserves_reason_in_the_wire_request() {
        let command:ParityCommand=serde_json::from_value(json!({"method":"set_room_affiliation","args":{
            "room_jid":"room@rooms.example.org","target_jid":"mallory@example.org","affiliation":"outcast","reason":"Repeated spam"
        }})).expect("typed command");
        let ParityCommand::SetRoomAffiliation {
            room_jid,
            target_jid,
            affiliation,
            reason,
        } = command
        else {
            panic!("affiliation command")
        };
        let iq = affiliation_request(&room_jid, &target_jid, affiliation, reason);
        let item = iq
            .get_child("query", waddle_xmpp_client::discovery::MUC_ADMIN_NS)
            .expect("query")
            .get_child("item", waddle_xmpp_client::discovery::MUC_ADMIN_NS)
            .expect("item");
        assert_eq!(item.attr("jid"), Some("mallory@example.org"));
        assert_eq!(item.attr("affiliation"), Some("outcast"));
        assert_eq!(
            item.get_child("reason", waddle_xmpp_client::discovery::MUC_ADMIN_NS)
                .expect("reason")
                .text(),
            "Repeated spam"
        );
    }

    #[test]
    fn xep0084_requests_share_content_id_and_publish_data_before_metadata() {
        let (id, data, metadata) =
            avatar_requests(b"PNG content", "image/png", 100, 50).expect("valid image");
        let data = data
            .get_child("pubsub", waddle_xmpp_client::avatar::NS_PUBSUB)
            .expect("pubsub")
            .get_child("publish", waddle_xmpp_client::avatar::NS_PUBSUB)
            .expect("publish");
        let metadata = metadata
            .get_child("pubsub", waddle_xmpp_client::avatar::NS_PUBSUB)
            .expect("pubsub")
            .get_child("publish", waddle_xmpp_client::avatar::NS_PUBSUB)
            .expect("publish");
        assert_eq!(data.attr("node"), Some(NS_AVATAR_DATA));
        assert_eq!(metadata.attr("node"), Some(NS_AVATAR_METADATA));
        assert_eq!(
            data.children().next().expect("item").attr("id"),
            Some(id.as_str())
        );
        assert_eq!(
            metadata.children().next().expect("item").attr("id"),
            Some(id.as_str())
        );
        assert!(avatar_requests(b"x", "image/jpeg", 0, 0).is_err());
        assert!(avatar_requests(b"x", "image/png", u32::MAX, 0).is_err());
    }

    #[test]
    fn xep0449_boundary_recomputes_pack_id_and_refuses_hashless_stickers() {
        let value = json!({"id":"untrusted-supplied-id","name":"Pack","summary":null,"restricted":false,"stickers":[{
            "desc":"🙂","media_type":"image/png","size":10,"width":10,"height":10,
            "hashes":[{"algo":"sha-256","value_b64":"YWJj"}],"sources":["https://example.org/sticker.png"],"suggests":[]
        }]});
        let pack = serde_json::from_value(value.clone()).expect("valid DTO");
        let doc = pack_from_boundary(pack).expect("valid pack");
        assert_ne!(doc.id.as_str(), "untrusted-supplied-id");
        let serialized = build_pack_element(&doc);
        assert_eq!(serialized.name(), "pack");
        let mut invalid = value;
        invalid["stickers"][0]["hashes"] = json!([]);
        assert!(pack_from_boundary(serde_json::from_value(invalid).expect("DTO")).is_err());
    }
}
