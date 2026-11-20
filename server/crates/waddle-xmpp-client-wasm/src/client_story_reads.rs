//! Per-user story read state — wasm bridge surface.
//!
//! Stored as a single PEP item on the user's own JID per XEP-0223.
//! Node options are committed via `<publish-options>` precondition on
//! every publish; see `waddle-xmpp-core::waddle_story_reads` and the
//! design spec at
//! `docs/superpowers/specs/2026-05-19-stories-media-and-reads-design.md`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_client::story_reads::{
    build_story_reads_fetch_iq, build_story_reads_publish_iq, parse_story_reads_fetch_result,
};
use waddle_xmpp_core::waddle_story_reads::{StoryId, StoryReads};
use wasm_bindgen::prelude::{wasm_bindgen, JsValue};

use super::{js_error, send_iq_command, to_js_value, WaddleClient};

/// JS-facing entry (id, RFC 3339 timestamp). `StoryId` is unwrapped at
/// this boundary — the wasm/JS layer carries it as a plain string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsStoryRead {
    pub id: String,
    pub at: String,
}

/// JS-facing read-state payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct JsStoryReads {
    pub entries: Vec<JsStoryRead>,
}

impl From<&StoryReads> for JsStoryReads {
    fn from(reads: &StoryReads) -> Self {
        Self {
            entries: reads
                .iter()
                .map(|(id, at)| JsStoryRead {
                    id: id.as_str().to_owned(),
                    at: at.to_rfc3339(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsStoryReadsInput {
    pub entries: Vec<JsStoryReadInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsStoryReadInput {
    pub id: String,
    pub at: String,
}

fn story_reads_from_js(input: JsStoryReadsInput) -> Result<StoryReads, JsValue> {
    let mut reads = StoryReads::default();
    for entry in input.entries {
        let id = StoryId::try_new(entry.id)
            .map_err(|err| js_error(format!("invalid story id: {err}")))?;
        let at = entry
            .at
            .parse::<DateTime<Utc>>()
            .map_err(|_| js_error(format!("invalid timestamp: {}", entry.at)))?;
        reads.mark_read(id, at);
    }
    Ok(reads)
}

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch the latest read-state from the user's own PEP node.
    ///
    /// Per XEP-0223 §Security Considerations (CVE-2023-28686), the
    /// result IQ's `from` attribute MUST be either absent or equal to
    /// the account's bare JID. Anything else is treated as spoofed
    /// and produces an empty result.
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
            let iq = build_story_reads_fetch_iq(&format!("story-reads-fetch-{}", Uuid::new_v4()));
            let result = match send_iq_command(inner, iq).await {
                Ok(r) => r,
                Err(_) => return to_js_value(&JsStoryReads::default()),
            };
            if let Some(from) = result.attr("from") {
                if from != own_bare {
                    return to_js_value(&JsStoryReads::default());
                }
            }
            let reads = parse_story_reads_fetch_result(&result);
            to_js_value(&JsStoryReads::from(&reads))
        })
    }

    /// Publish the user's read-state. Overwrites the single `current`
    /// item on every call.
    pub fn story_reads_publish(&self, input: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let input: JsStoryReadsInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid story_reads input: {err}")))?;
            let reads = story_reads_from_js(input)?;
            let iq = build_story_reads_publish_iq(
                &format!("story-reads-publish-{}", Uuid::new_v4()),
                &reads,
            );
            send_iq_command(inner, iq).await?;
            to_js_value(&JsStoryReads::from(&reads))
        })
    }
}
