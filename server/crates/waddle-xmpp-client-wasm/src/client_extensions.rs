//! Browser discovery of extension routes and retrieval of their PubSub-backed items.
use super::{
    future_to_promise, js_error, send_iq_command, to_js_value, wasm_bindgen, JsValue, Promise,
    WaddleClient,
};
use jid::{BareJid, Jid};
use waddle_xmpp_client_community::extension_route_types::WaddleExtensionRoute;
use waddle_xmpp_client_community::extension_routes::{self, RouteError};

fn route_error(error: RouteError<JsValue>) -> JsValue {
    match error {
        RouteError::Transport(e) => e,
        RouteError::Invalid => js_error("invalid extension route"),
        RouteError::Malformed => js_error("unexpected extension route response or node"),
        RouteError::Untrusted => js_error("unexpected extension service sender"),
    }
}

#[wasm_bindgen]
impl WaddleClient {
    /// Discover the channel extension routes advertised for this account.
    pub fn discover_extension_routes(&self, user_jid: Option<String>) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let id: Jid = user_jid
                .unwrap_or_else(|| inner.borrow().config.jid.clone())
                .parse()
                .map_err(|e: jid::Error| js_error(e.to_string()))?;
            let routes = extension_routes::discover(
                |iq| send_iq_command(inner.clone(), iq),
                id.domain().as_str(),
            )
            .await
            .map_err(route_error)?;
            to_js_value(&routes)
        })
    }

    /// Read up to 100 items from the selected channel's route node.
    pub fn fetch_extension_route_items(&self, route: JsValue, room_jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let route: WaddleExtensionRoute =
                serde_wasm_bindgen::from_value(route).map_err(|e| js_error(e.to_string()))?;
            let room: BareJid = room_jid
                .parse()
                .map_err(|e: jid::Error| js_error(e.to_string()))?;
            let items =
                extension_routes::items(|iq| send_iq_command(inner.clone(), iq), &route, &room)
                    .await
                    .map_err(route_error)?;
            to_js_value(&items)
        })
    }
}
