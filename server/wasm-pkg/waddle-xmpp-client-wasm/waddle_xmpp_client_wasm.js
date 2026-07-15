/* @ts-self-types="./waddle_xmpp_client_wasm.d.ts" */
import wasmUrl from "./waddle_xmpp_client_wasm_bg.wasm?url&b=9e1cb753a53bb3461e528ddd062079efcdc7e0033faa37522cfcfbf5c640a227";
import * as bgModule from "./waddle_xmpp_client_wasm_bg.js?b=9e1cb753a53bb3461e528ddd062079efcdc7e0033faa37522cfcfbf5c640a227";
import { __wbg_set_wasm } from "./waddle_xmpp_client_wasm_bg.js?b=9e1cb753a53bb3461e528ddd062079efcdc7e0033faa37522cfcfbf5c640a227";

let initPromise;

export default async function init() {
  if (!initPromise) {
    initPromise = (async () => {
      // In dev mode bypass the browser HTTP/WebAssembly cache so that a fresh
      // REBUILD_WASM=1 build is picked up without a manual hard-refresh.
      // In production the URL is content-hashed, so "default" is fine.
      const cache = import.meta.env.DEV ? "no-store" : "default";
      const response = await fetch(wasmUrl, { cache });
      const bytes = await response.arrayBuffer();
      const { instance } = await WebAssembly.instantiate(bytes, {
        // The import-object key must match the literal string the WASM binary
        // imports from — wasm-pack writes "./waddle_xmpp_client_wasm_bg.js"
        // into the binary, with no query string.
        "./waddle_xmpp_client_wasm_bg.js": bgModule,
      });
      __wbg_set_wasm(instance.exports);
    })();
  }
  return initPromise;
}

// Re-export every public binding wasm-pack emitted — classes (WaddleClient,
// WaddleConfig, …) AND Rust free functions (xep0392_consistent_hue,
// xep0392_consistent_color, …). A hand-curated list silently drops new
// #[wasm_bindgen] free functions until somebody notices the chat crashing.
export * from "./waddle_xmpp_client_wasm_bg.js?b=9e1cb753a53bb3461e528ddd062079efcdc7e0033faa37522cfcfbf5c640a227";
