mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "waddle-extension",
        with: {
            "wasi:logging/logging@0.1.0-draft": generate,
            "wasi:clocks/monotonic-clock@0.2.0": generate,
            "wasi:io/poll@0.2.0": generate,
            "wasi:random/random@0.2.0": generate,
        },
    });
}

mod command;
mod constants;
mod manifest;
mod quotes;
mod ui;

use bindings::exports;
use bindings::waddle::extension::types;
use command::handle_command;

struct StargateQuotes;

bindings::export!(StargateQuotes with_types_in bindings);

impl exports::waddle::extension::lifecycle::Guest for StargateQuotes {
    fn init(_config: String) -> Result<types::ExtensionManifest, String> {
        quotes::quote_catalog().map_err(|error| error.message.value)?;
        Ok(manifest::manifest())
    }
}

impl exports::waddle::extension::framework::Guest for StargateQuotes {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::Command(command) => handle_command(command)?,
            types::ExtensionEvent::MessageHook(_)
            | types::ExtensionEvent::Launch(_)
            | types::ExtensionEvent::ProviderWebhook(_) => vec![],
        };
        Ok(types::ExtensionResponse { effects })
    }
}

#[cfg(test)]
pub(crate) use command::{handle_command_with_rng, take_sent_room_messages};
#[cfg(test)]
pub(crate) use constants::{COMMAND_NAME, COMMAND_NODE, COMMAND_RESULT_TEXT, PLUGIN_NS};
#[cfg(test)]
pub(crate) use manifest::manifest;
#[cfg(test)]
pub(crate) use quotes::{parse_quotes_json, quote_body, quote_catalog, select_quote_with_rng};

#[cfg(test)]
mod tests;
