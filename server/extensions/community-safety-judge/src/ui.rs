use crate::bindings::waddle::extension::types;
use crate::constants::PLUGIN_ID;

pub(crate) fn plugin_id() -> types::PluginId {
    types::PluginId {
        value: PLUGIN_ID.to_string(),
    }
}

pub(crate) fn display(value: &str) -> types::DisplayText {
    types::DisplayText {
        value: value.to_string(),
    }
}
