use std::ffi::{c_char, CStr, CString};
use std::ptr;

use crate::runtime;

const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;

/// Allocate an independent native client runtime. Zero means initialization failed.
#[no_mangle]
pub extern "C" fn waddle_flutter_create() -> u64 {
    runtime::create()
}

/// Submit one UTF-8 JSON request without waiting for network I/O.
/// A positive ID identifies its result in poll. Zero means no request was accepted.
/// Invalid methods/arguments on a live handle produce a normal asynchronous error result.
///
/// # Safety
/// `request_json` must point to a readable, NUL-terminated UTF-8 JSON buffer for this call.
#[no_mangle]
pub unsafe extern "C" fn waddle_flutter_submit(handle: u64, request_json: *const c_char) -> u64 {
    if request_json.is_null() {
        return 0;
    }
    let Some(runtime) = runtime::lookup(handle) else {
        return 0;
    };
    // SAFETY: The caller guarantees this input remains readable and NUL-terminated for the call.
    let bytes = unsafe { CStr::from_ptr(request_json) }.to_bytes();
    if bytes.len() > MAX_REQUEST_BYTES {
        return 0;
    }
    runtime.submit(bytes)
}

/// Drain up to 128 result/event envelopes as an owned UTF-8 JSON array.
/// Release every non-null return value exactly once using `waddle_flutter_free_json`.
#[no_mangle]
pub extern "C" fn waddle_flutter_poll(handle: u64) -> *mut c_char {
    let frames = runtime::lookup(handle)
        .map(|runtime| runtime.poll())
        .unwrap_or_default();
    let json = serde_json::to_string(&frames).unwrap_or_else(|_| "[]".to_owned());
    match CString::new(json) {
        Ok(buffer) => buffer.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// Free one owned buffer returned by this library. Null is accepted.
///
/// # Safety
/// The non-null pointer must have been returned by `waddle_flutter_poll`, and must not have
/// been freed before. The caller must not retain or access the buffer after this call.
#[no_mangle]
pub unsafe extern "C" fn waddle_flutter_free_json(buffer: *mut c_char) {
    if !buffer.is_null() {
        // SAFETY: The caller transfers back the exact allocation created with CString::into_raw.
        drop(unsafe { CString::from_raw(buffer) });
    }
}

/// Retire a handle and start asynchronous disconnection. Repeated destruction is harmless.
/// All unpolled results/events are discarded; the Dart adapter cancels its pending futures.
#[no_mangle]
pub extern "C" fn waddle_flutter_destroy(handle: u64) {
    runtime::destroy(handle);
}

#[cfg(test)]
mod tests {
    use super::{
        waddle_flutter_create, waddle_flutter_destroy, waddle_flutter_free_json,
        waddle_flutter_poll, waddle_flutter_submit,
    };
    use std::ffi::{CStr, CString};

    #[test]
    fn real_c_abi_owns_buffers_and_drives_shared_timeline_without_network() {
        let handle = waddle_flutter_create();
        assert_ne!(handle, 0);
        let request = CString::new(r#"{"method":"timeline_snapshot","args":{"conversation_jid":"room@conference.example.test"}}"#).expect("json");
        // SAFETY: request is a valid CString that remains alive through submit.
        let id = unsafe { waddle_flutter_submit(handle, request.as_ptr()) };
        assert_ne!(id, 0);
        let buffer = waddle_flutter_poll(handle);
        assert!(!buffer.is_null());
        // SAFETY: poll transfers ownership of a valid NUL-terminated buffer.
        let json = unsafe { CStr::from_ptr(buffer) }.to_str().expect("utf8");
        let result: serde_json::Value = serde_json::from_str(json).expect("json");
        assert_eq!(result[0]["request_id"], id);
        assert_eq!(result[0]["ok"], true);
        // SAFETY: buffer is returned once, and never accessed after this call.
        unsafe { waddle_flutter_free_json(buffer) };
        waddle_flutter_destroy(handle);
        waddle_flutter_destroy(handle);
        // SAFETY: request remains readable, but its handle is retired, so no request is accepted.
        let retired = unsafe { waddle_flutter_submit(handle, request.as_ptr()) };
        assert_eq!(retired, 0);
    }
}
