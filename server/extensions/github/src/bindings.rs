wit_bindgen::generate!({
    path: "../../wit",
    world: "waddle-extension",
    with: {
        "wasi:logging/logging@0.1.0-draft": generate,
        "wasi:clocks/monotonic-clock@0.2.0": generate,
        "wasi:io/poll@0.2.0": generate,
        "wasi:random/random@0.2.0": generate,
    },
});
