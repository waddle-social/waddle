//! Nonblocking, owned-buffer C ABI for Flutter. Protocol operations remain in the shared client.
//!
//! A handle owns a client generation, bounded command/event queues and async request tasks.
//! Callers poll owned JSON buffers and release each exactly once with `waddle_flutter_free_json`.

mod abi;
#[cfg(any(target_os = "linux", target_os = "android"))]
mod audio_abi;
#[cfg(target_os = "android")]
mod audio_android;
#[cfg(any(target_os = "linux", target_os = "android"))]
mod camera_abi;
#[cfg(target_os = "android")]
mod camera_android;
mod error;
mod events;
mod generated;
mod request;
mod runtime;

pub use abi::{
    waddle_flutter_create, waddle_flutter_destroy, waddle_flutter_free_json, waddle_flutter_poll,
    waddle_flutter_submit,
};
