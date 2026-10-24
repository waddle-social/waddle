//! Private generated WIT imports and exports used to bridge Wasmtime to host types.

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "waddle-extension",
    imports: { default: async | tracing | trappable },
    exports: { default: async },
    with: {
        "waddle:extension/delivery@3.0.0.delivery-key": crate::runtime::DeliveryKey,
        "wasi:io": wasmtime_wasi::p2::bindings::io,
        "wasi:clocks": wasmtime_wasi::p2::bindings::clocks,
        "wasi:random": wasmtime_wasi::p2::bindings::random,
    },
});
