mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "waddle-extension",
        with: {
            "wasi:logging/logging@0.1.0-draft": generate,
            "wasi:clocks/monotonic-clock@0.2.0": generate,
            "wasi:io/poll@0.2.0": generate,
        },
    });
}

use bindings::exports;
use bindings::waddle::extension::types;

mod enrichment;
mod helpers;
mod manifest;
mod payloads;

use enrichment::{link_enrichments, save_link};
use helpers::*;
use manifest::manifest;
use payloads::*;

struct LinkBoard;

bindings::export!(LinkBoard with_types_in bindings);

const PLUGIN_ID: &str = "link-board";
const PLUGIN_NAME: &str = "Link Board";
const PLUGIN_NS: &str = "urn:waddle:link-board:1";
const FRAMEWORK_NS: &str = "urn:waddle:extension:1";
const EXTENSION_ITEM_ROOT: &str = "extension-item";
const INVOKE_NODE: &str = "urn:waddle:extension:1:invoke";
const VERSION: &str = "0.1.0";

impl exports::waddle::extension::lifecycle::Guest for LinkBoard {
    fn init(_config: String) -> Result<types::ExtensionManifest, String> {
        Ok(manifest())
    }
}

impl exports::waddle::extension::framework::Guest for LinkBoard {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::MessageHook(hook) => link_enrichments(&hook),
            types::ExtensionEvent::Launch(launch) => save_link(launch),
            types::ExtensionEvent::Command(_) => vec![],
            types::ExtensionEvent::ProviderWebhook(_) => vec![],
        };
        Ok(types::ExtensionResponse { effects })
    }
}

#[cfg(test)]
mod tests;
