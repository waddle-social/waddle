//! XEP-0501 Pubsub Stories — wasm bridge surface.
//!
//! The server bootstraps a community stories node at
//! `urn:xmpp:pubsub-social-feed:stories:0` on the community service. These wasm methods
//! let the chat read active stories (`stories_items`, returns ALL
//! items; the chat filters expired locally) and publish new stories
//! with an expiry (`stories_publish`).

use waddle_xmpp_client_community::stories::{
    build_stories_items_iq, parse_stories_items_result, prepare_publication, WaddleStoryInput,
};
use wasm_bindgen::prelude::{wasm_bindgen, JsValue};

use super::{js_error, send_iq_command, service_bare_jid, to_js_value, WaddleClient};

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch the latest stories from the community stories node on
    /// `community_jid`. Returns ALL items including expired ones —
    /// the chat filters active vs expired locally so a story fades
    /// out as the countdown hits zero without a server roundtrip.
    pub fn stories_items(&self, community_jid: String, max_items: Option<u32>) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let service = service_bare_jid(&community_jid)?;
            let iq = build_stories_items_iq(&service, max_items);
            let result = send_iq_command(inner, iq).await?;
            let stories = parse_stories_items_result(&result);
            to_js_value(&stories)
        })
    }

    /// Publish a new story. `media_url` is required by XEP-0501; `body`
    /// is optional text content attached to that media. `expiry_hours`
    /// defaults to 24.
    ///
    pub fn stories_publish(&self, community_jid: String, input: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let service = service_bare_jid(&community_jid)?;
            let input: WaddleStoryInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid story input: {err}")))?;
            let (iq, published) =
                prepare_publication(Some(&service), input).map_err(|e| js_error(e.to_string()))?;
            send_iq_command(inner, iq).await?;
            to_js_value(&published)
        })
    }
}
