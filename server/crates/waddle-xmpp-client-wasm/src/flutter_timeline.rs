//! Browser binding for the same trusted timeline adapter used by native Flutter.

use std::cell::RefCell;

use waddle_flutter_timeline::{TimelineCommand, TimelineStore};
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::JsValue;

/// An account-scoped, sans-IO message timeline for a Flutter transport.
#[wasm_bindgen]
#[derive(Default)]
pub struct WaddleFlutterTimeline {
    inner: RefCell<TimelineStore>,
}

#[wasm_bindgen]
impl WaddleFlutterTimeline {
    /// Create an empty store. No connection or browser global state is retained.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Execute a typed native-compatible `timeline_*` request.
    ///
    /// # Errors
    /// Rejects malformed requests and attempts to mix account generations.
    pub fn execute(&self, request: JsValue) -> Result<JsValue, JsValue> {
        let command: TimelineCommand = serde_wasm_bindgen::from_value(request)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let snapshot = self
            .inner
            .borrow_mut()
            .execute(command)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        serde_wasm_bindgen::to_value(&snapshot)
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }
}
