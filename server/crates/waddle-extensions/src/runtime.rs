wasmtime::component::bindgen!({
    path: "../../wit",
    world: "waddle-extension",
    imports: { default: async | tracing | trappable },
    exports: { default: async },
    with: {
        "wasi:io": wasmtime_wasi::p2::bindings::io,
        "wasi:clocks": wasmtime_wasi::p2::bindings::clocks,
    },
});

macro_rules! domain_newtype_to_wit {
    ($value:expr, $wit:ident) => {
        wit_types::$wit {
            value: $value.as_str().to_string(),
        }
    };
}

macro_rules! wit_newtype_to_domain {
    ($value:expr, $domain:ty) => {
        <$domain>::new($value.value).map_err(anyhow::Error::from)
    };
}

mod domain_to_wit;
mod host_state;
mod host_tool_conversions;
mod http;
mod loader;
#[cfg(test)]
mod tests;
mod ui_conversions;
mod wit_to_domain;

pub use host_state::HostState;
pub use loader::{LoadedExtension, WasmRuntime};
