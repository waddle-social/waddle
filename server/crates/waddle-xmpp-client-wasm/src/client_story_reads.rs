//! Per-user story read state — wasm bridge surface.
//!
//! Stored as a single PEP item on the user's own JID per XEP-0223.
//! Node options are committed via `<publish-options>` precondition on
//! every publish; see `waddle-xmpp-core::waddle_story_reads` and the
//! design spec at
//! `docs/superpowers/specs/2026-05-19-stories-media-and-reads-design.md`.

use waddle_xmpp_client_community::story_reads::{
    build_story_reads_fetch_iq, build_story_reads_publish_iq, parse_story_reads_fetch_result,
    story_reads_from_input, WaddleStoryReads, WaddleStoryReadsInput,
};
use wasm_bindgen::prelude::{wasm_bindgen, JsValue};

use super::{js_error, send_iq_command, send_iq_command_stanza_aware, to_js_value, WaddleClient};

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch the latest read-state from the user's own PEP node.
    ///
    /// Per XEP-0223 §Security Considerations (CVE-2023-28686), the
    /// result IQ's `from` attribute MUST be either absent or equal to
    /// the account's bare JID. Anything else is treated as spoofed
    /// and rejected.
    pub fn story_reads_fetch(&self) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let own_bare = {
                let jid_str = inner.borrow().config.jid.clone();
                let parsed: jid::Jid = jid_str
                    .parse()
                    .map_err(|err| js_error(format!("invalid own JID: {err}")))?;
                parsed.to_bare().to_string()
            };
            let iq = build_story_reads_fetch_iq();
            let result = match send_iq_command_stanza_aware(inner, iq).await? {
                Ok(r) => r,
                Err(err) if err.condition == "item-not-found" => {
                    return to_js_value(&WaddleStoryReads::default())
                }
                Err(err) => return Err(js_error(err.to_string())),
            };
            if let Some(from) = result.attr("from") {
                if from != own_bare {
                    return Err(js_error("unexpected story read-state sender"));
                }
            }
            let reads = parse_story_reads_fetch_result(&result);
            to_js_value(&WaddleStoryReads::from(&reads))
        })
    }

    /// Publish the user's read-state. Overwrites the single `current`
    /// item on every call.
    pub fn story_reads_publish(&self, input: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let input: WaddleStoryReadsInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid story_reads input: {err}")))?;
            let reads = story_reads_from_input(input).map_err(|e| js_error(e.to_string()))?;
            let iq = build_story_reads_publish_iq(&reads);
            send_iq_command(inner, iq).await?;
            to_js_value(&WaddleStoryReads::from(&reads))
        })
    }
}
