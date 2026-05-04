/* @ts-self-types="./waddle_xmpp_client_wasm.d.ts" */
import initWasm from "./waddle_xmpp_client_wasm_bg.wasm?init";
import * as bgModule from "./waddle_xmpp_client_wasm_bg.js";
import { __wbg_set_wasm } from "./waddle_xmpp_client_wasm_bg.js";

let initPromise;

export default async function init() {
  if (!initPromise) {
    initPromise = initWasm({ "./waddle_xmpp_client_wasm_bg.js": bgModule }).then((instance) => {
      __wbg_set_wasm(instance.exports);
    });
  }
  return initPromise;
}

export { WaddleClient, WaddleConfig } from "./waddle_xmpp_client_wasm_bg.js";
