//! XEP-0501 per-author PEP story bindings.
use jid::{BareJid, Jid};
use js_sys::Promise;
use waddle_xmpp_client_community::pep_stories;
use wasm_bindgen::prelude::{wasm_bindgen, JsValue};
use wasm_bindgen_futures::future_to_promise;

use super::client_account::WasmCommandDriver;
use super::{js_error, service_bare_jid, to_js_value, WaddleClient};

impl WaddleClient {
    fn story_session(&self) -> Result<(WasmCommandDriver, BareJid), JsValue> {
        let own = self
            .inner
            .borrow()
            .config
            .jid
            .parse::<Jid>()
            .map_err(|e| js_error(e.to_string()))?
            .to_bare();
        Ok((
            WasmCommandDriver {
                inner: self.inner.clone(),
            },
            own,
        ))
    }
}

#[wasm_bindgen]
impl WaddleClient {
    /// List the account and contacts whose presence it receives.
    pub fn story_authors(&self) -> Promise {
        let session = self.story_session();
        future_to_promise(async move {
            let (driver, own) = session?;
            to_js_value(
                &pep_stories::authors(&driver, &own)
                    .await
                    .map_err(|e| js_error(e.to_string()))?,
            )
        })
    }

    /// Read stories from one author’s PEP node.
    pub fn fetch_stories(&self, author_jid: String, max_items: Option<u32>) -> Promise {
        let session = self.story_session();
        future_to_promise(async move {
            let (driver, own) = session?;
            let author = service_bare_jid(&author_jid)?;
            to_js_value(
                &pep_stories::fetch(&driver, &author, &own, max_items)
                    .await
                    .map_err(|e| js_error(e.to_string()))?,
            )
        })
    }

    /// Publish a story to this account’s PEP node.
    pub fn publish_story(&self, input: JsValue) -> Promise {
        let session = self.story_session();
        future_to_promise(async move {
            let (driver, own) = session?;
            let input =
                serde_wasm_bindgen::from_value(input).map_err(|e| js_error(e.to_string()))?;
            to_js_value(
                &pep_stories::publish(&driver, &own, input)
                    .await
                    .map_err(|e| js_error(e.to_string()))?,
            )
        })
    }

    /// Fetch aggregate reactions and this account’s emoji for one story.
    pub fn fetch_story_reactions(&self, author_jid: String, story_id: String) -> Promise {
        let session = self.story_session();
        future_to_promise(async move {
            let (driver, own) = session?;
            let author = service_bare_jid(&author_jid)?;
            to_js_value(
                &pep_stories::reactions(&driver, &author, &story_id, &own)
                    .await
                    .map_err(|e| js_error(e.to_string()))?,
            )
        })
    }

    /// Replace this account’s reaction set on one story.
    pub fn set_story_reactions(
        &self,
        author_jid: String,
        story_id: String,
        emojis: Vec<String>,
    ) -> Promise {
        let session = self.story_session();
        future_to_promise(async move {
            let (driver, own) = session?;
            let author = service_bare_jid(&author_jid)?;
            pep_stories::set_reactions(&driver, &author, &story_id, &own, emojis)
                .await
                .map_err(|e| js_error(e.to_string()))?;
            Ok(JsValue::UNDEFINED)
        })
    }
}
