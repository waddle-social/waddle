#![cfg(feature = "wasm")]

use futures::channel::mpsc::{self, Receiver};
use js_sys::Array;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

#[derive(Debug)]
pub struct WasmWebSocket {
    ws: web_sys::WebSocket,
    pub rx: Receiver<WasmTransportEvent>,
    _onopen: Closure<dyn FnMut()>,
    _onmessage: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _onclose: Closure<dyn FnMut(web_sys::CloseEvent)>,
    _onerror: Closure<dyn FnMut(web_sys::ErrorEvent)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmTransportEvent {
    Open,
    Message(String),
    Close { code: u16, reason: String },
    Error,
}

impl WasmWebSocket {
    pub fn connect(url: &str) -> Result<Self, wasm_bindgen::JsValue> {
        let protocols = Array::new();
        protocols.push(&wasm_bindgen::JsValue::from_str("xmpp"));

        let ws = web_sys::WebSocket::new_with_str_sequence(url, &protocols)?;
        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

        let (tx, rx) = mpsc::channel::<WasmTransportEvent>(256);

        let mut open_tx = tx.clone();
        let open_ws = ws.clone();
        let onopen = Closure::wrap(Box::new(move || {
            if open_tx.try_send(WasmTransportEvent::Open).is_err() {
                let _ = open_ws.close();
            }
        }) as Box<dyn FnMut()>);
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));

        let mut message_tx = tx.clone();
        let message_ws = ws.clone();
        let onmessage = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
            if let Some(text) = event.data().as_string() {
                if message_tx
                    .try_send(WasmTransportEvent::Message(text))
                    .is_err()
                {
                    let _ = message_ws.close();
                }
            } else {
                let _ = message_tx.try_send(WasmTransportEvent::Error);
                let _ = message_ws.close();
            }
        }) as Box<dyn FnMut(web_sys::MessageEvent)>);
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

        let mut close_tx = tx.clone();
        let close_ws = ws.clone();
        let onclose = Closure::wrap(Box::new(move |event: web_sys::CloseEvent| {
            if close_tx
                .try_send(WasmTransportEvent::Close {
                    code: event.code(),
                    reason: event.reason(),
                })
                .is_err()
            {
                let _ = close_ws.close();
            }
        }) as Box<dyn FnMut(web_sys::CloseEvent)>);
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));

        let mut error_tx = tx;
        let error_ws = ws.clone();
        let onerror = Closure::wrap(Box::new(move |_event: web_sys::ErrorEvent| {
            if error_tx.try_send(WasmTransportEvent::Error).is_err() {
                let _ = error_ws.close();
            }
        }) as Box<dyn FnMut(web_sys::ErrorEvent)>);
        ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));

        Ok(Self {
            ws,
            rx,
            _onopen: onopen,
            _onmessage: onmessage,
            _onclose: onclose,
            _onerror: onerror,
        })
    }

    pub fn web_socket(&self) -> &web_sys::WebSocket {
        &self.ws
    }

    pub fn send(&self, frame: &str) -> Result<(), wasm_bindgen::JsValue> {
        self.ws.send_with_str(frame)
    }

    pub fn close(&self) -> Result<(), wasm_bindgen::JsValue> {
        self.ws.close()
    }
}

impl Drop for WasmWebSocket {
    fn drop(&mut self) {
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        self.ws.set_onerror(None);
    }
}
