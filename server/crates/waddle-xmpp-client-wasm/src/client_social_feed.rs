//! XEP-0472 Pubsub Social Feed — wasm bridge surface.
//!
//! The server bootstraps a community feed node at
//! `urn:xmpp:pubsub-social-feed:1` on the community service
//! (`community.<domain>`). These wasm methods let the chat read the
//! feed (`feed_items`) and publish new posts (`feed_publish`) via the
//! shared XEP-0060 pubsub helpers in `waddle_xmpp_client::pubsub`,
//! with the typed XEP-0472 `<entry/>` payload constructed / parsed in
//! Rust via `waddle_xmpp_core::xep0472`.

use waddle_xmpp_client_community::feed::{
    build_feed_items_iq, parse_feed_items_result, prepare_publication, WaddleFeedEntryInput,
};
use wasm_bindgen::prelude::{wasm_bindgen, JsValue};

use super::{js_error, send_iq_command, service_bare_jid, to_js_value, WaddleClient};

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch the latest items from the community Social Feed node on
    /// `spaces_jid` — the community service (`community.<domain>`).
    /// Returns an array of WaddleFeedEntry objects ordered as the server
    /// delivered them (newest first by `last_published`).
    pub fn feed_items(&self, spaces_jid: String, max_items: Option<u32>) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let service = service_bare_jid(&spaces_jid)?;
            let iq = build_feed_items_iq(&service, max_items);
            let result = send_iq_command(inner, iq).await?;
            let entries = parse_feed_items_result(&result);
            to_js_value(&entries)
        })
    }

    /// Publish a new entry to the community Social Feed. The server
    /// enforces publish authorisation via XEP-0060 affiliations;
    /// callers without Publisher access receive a Forbidden stanza
    /// error which surfaces as a rejected Promise.
    ///
    pub fn feed_publish(&self, spaces_jid: String, entry: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let service = service_bare_jid(&spaces_jid)?;
            let input: WaddleFeedEntryInput = serde_wasm_bindgen::from_value(entry)
                .map_err(|err| js_error(format!("invalid feed entry input: {err}")))?;
            let (iq, published) = prepare_publication(&service, input);
            send_iq_command(inner, iq).await?;
            to_js_value(&published)
        })
    }
}
