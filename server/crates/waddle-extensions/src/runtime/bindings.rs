//! Private generated WIT imports and exports used to bridge Wasmtime to host types.

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "waddle-extension",
    imports: { default: async | tracing | trappable },
    exports: { default: async },
    with: {
        "waddle:extension/delivery@4.0.0.delivery-key": crate::runtime::DeliveryKey,
        "waddle:extension/inference@4.0.0.inference-stream": crate::runtime::InferenceStream,
        "wasi:io": wasmtime_wasi::p2::bindings::io,
        "wasi:clocks": wasmtime_wasi::p2::bindings::clocks,
        "wasi:random": wasmtime_wasi::p2::bindings::random,
    },
});
