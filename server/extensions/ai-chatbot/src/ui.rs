#[cfg(not(test))]
use crate::bindings::waddle::extension::runtime;
use crate::bindings::waddle::extension::types;
use crate::constants::{PLUGIN_ID, PLUGIN_NS};

pub(crate) fn plugin_id() -> types::PluginId {
    types::PluginId {
        value: PLUGIN_ID.to_string(),
    }
}

pub(crate) fn payload_namespace() -> types::PayloadNamespace {
    types::PayloadNamespace {
        value: PLUGIN_NS.to_string(),
    }
}

pub(crate) fn display(value: &str) -> types::DisplayText {
    types::DisplayText {
        value: value.to_string(),
    }
}

pub(crate) fn timestamp() -> types::Timestamp {
    types::Timestamp {
        value: current_timestamp_value(),
    }
}

#[cfg(not(test))]
fn current_timestamp_value() -> String {
    runtime::current_timestamp()
}

#[cfg(test)]
fn current_timestamp_value() -> String {
    "1970-01-01T00:00:00Z".to_string()
}
