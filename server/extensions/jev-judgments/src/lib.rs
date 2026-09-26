//! Jev runs as a room observer; the host owns subscriptions and publication.
#[cfg(any(target_arch = "wasm32", test))]
mod bindings {
    wit_bindgen::generate!({
        path: "../../wit", world: "waddle-extension",
        with: {
            "wasi:logging/logging@0.1.0-draft": generate,
            "wasi:clocks/monotonic-clock@0.2.0": generate,
            "wasi:io/poll@0.2.0": generate,
            "wasi:random/random@0.2.0": generate,
        },
    });
}
pub mod config;
pub mod decisions;
#[cfg(target_arch = "wasm32")]
mod guest;
#[cfg(any(target_arch = "wasm32", test))]
mod payload;
#[cfg(test)]
mod tests;
