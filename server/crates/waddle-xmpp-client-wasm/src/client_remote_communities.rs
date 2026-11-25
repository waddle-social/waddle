//! Browser bindings for the same host-authoritative remote-community operations as native.
use super::{
    future_to_promise, js_error, send_iq_command, send_iq_command_stanza_aware, to_js_value,
    wasm_bindgen, JsValue, Promise, WaddleClient, WaddleClientInner,
};
use crate::commands::send_federated_iq_command;
use jid::{BareJid, Jid};
use waddle_xmpp_client_community::{remote_community as remote, remote_preferences as prefs};

fn owner(inner: &WaddleClientInner) -> Result<BareJid, JsValue> {
    inner
        .config
        .jid
        .parse::<Jid>()
        .map(|j| j.to_bare())
        .map_err(|e| js_error(e.to_string()))
}
fn error(error: remote::Error<JsValue>) -> JsValue {
    match error {
        remote::Error::Invalid => js_error("Invalid federation address"),
        remote::Error::Unexpected => js_error("Unexpected community reply or node"),
        remote::Error::Untrusted => js_error("Unexpected community reply sender or subscriber"),
        remote::Error::Transport(e) => e,
    }
}

#[wasm_bindgen]
impl WaddleClient {
    /// Query the addressed room, Space or directory and validate its host and node.
    pub fn describe_remote_community(&self, address: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let result =
                remote::describe(|iq| send_federated_iq_command(inner.clone(), iq), &address)
                    .await
                    .map_err(error)?;
            to_js_value(&result)
        })
    }
    /// Change the bare account’s XEP-0060 subscription at the Space host.
    pub fn change_remote_subscription(
        &self,
        address: String,
        follow: bool,
        subid: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let own = owner(&inner.borrow())?;
            let result = remote::subscribe(
                |iq| send_federated_iq_command(inner.clone(), iq),
                &own,
                &address,
                follow,
                subid.as_deref(),
            )
            .await
            .map_err(error);
            to_js_value(&result?)
        })
    }
    /// Read qualified Space preferences from the account’s private PEP node.
    pub fn fetch_saved_remote_spaces(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let own = owner(&inner.borrow())?;
            let load = || {
                let inner = inner.clone();
                let own = &own;
                async move {
                    match send_iq_command_stanza_aware(inner, prefs::list_iq()).await? {
                        Ok(result) => prefs::parse(&result, own)
                            .ok_or_else(|| js_error("Unexpected saved Spaces reply")),
                        Err(e) if e.condition == "item-not-found" => Ok(vec![]),
                        Err(e) => Err(js_error(e.to_string())),
                    }
                }
            };
            let entries = load().await?;
            let entries = waddle_xmpp_client_community::remote_legacy::migrate(
                |iq| send_iq_command(inner.clone(), iq),
                load,
                &own,
                entries,
            )
            .await;
            to_js_value(&entries)
        })
    }
    /// Publish or retract one private Space preference without changing membership.
    pub fn update_saved_remote_space(&self, entry: JsValue, forget: bool) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let own = owner(&inner.borrow())?;
            let entry: prefs::WaddleSavedSpace =
                serde_wasm_bindgen::from_value(entry).map_err(|e| js_error(e.to_string()))?;
            prefs::update(
                |iq| send_iq_command(inner.clone(), iq),
                &own,
                &entry,
                forget,
            )
            .await
            .map_err(error)?;
            Ok(JsValue::UNDEFINED)
        })
    }
    /// Set autojoin or forget a single room in the account’s XEP-0402 bookmarks.
    pub fn update_remote_room_bookmark(
        &self,
        room_jid: String,
        name: Option<String>,
        autojoin: bool,
        forget: bool,
    ) -> Promise {
        use waddle_xmpp_client_community::remote_bookmark;
        let inner = self.inner.clone();
        future_to_promise(async move {
            let own = owner(&inner.borrow())?;
            let result =
                match send_iq_command_stanza_aware(inner.clone(), remote_bookmark::read_iq())
                    .await?
                {
                    Ok(r) => Some(r),
                    Err(e) if e.condition == "item-not-found" => None,
                    Err(e) => return Err(js_error(e.to_string())),
                };
            remote_bookmark::update(
                |iq| send_iq_command(inner.clone(), iq),
                &own,
                &room_jid,
                name,
                autojoin,
                forget,
                result.as_ref(),
            )
            .await
            .map_err(error)?;
            Ok(JsValue::UNDEFINED)
        })
    }
    /// Send join or leave presence. The room host confirms occupancy with XEP-0045 status 110.
    pub fn request_remote_room_presence(
        &self,
        room_jid: String,
        nick: String,
        join: bool,
    ) -> Promise {
        if join {
            self.join_room(room_jid, nick)
        } else {
            self.leave_room(room_jid, nick)
        }
    }
}
