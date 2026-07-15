import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

export function renderWasmWrapper(buildId) {
	if (!/^[0-9a-f]{64}$/u.test(buildId)) {
		throw new Error("WASM build ID must be a full lowercase SHA-256 digest");
	}
	return `/* @ts-self-types="./waddle_xmpp_client_wasm.d.ts" */
import wasmUrl from "./waddle_xmpp_client_wasm_bg.wasm?url&b=${buildId}";
import * as bgModule from "./waddle_xmpp_client_wasm_bg.js?b=${buildId}";
import { __wbg_set_wasm } from "./waddle_xmpp_client_wasm_bg.js?b=${buildId}";

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
export * from "./waddle_xmpp_client_wasm_bg.js?b=${buildId}";
`;
}

export function finalizeWasmPackage(outDir, buildId) {
	const pkgJsonPath = resolve(outDir, "package.json");
	const pkg = JSON.parse(readFileSync(pkgJsonPath, "utf8"));
	pkg.name = "@waddle/xmpp-client-wasm";
	pkg.publishConfig = {
		registry: "https://npm.pkg.github.com",
		access: "public",
	};
	writeFileSync(pkgJsonPath, `${JSON.stringify(pkg, null, 2)}\n`);

	writeFileSync(
		resolve(outDir, "waddle_xmpp_client_wasm.js"),
		renderWasmWrapper(buildId),
	);

	const dtsPath = resolve(outDir, "waddle_xmpp_client_wasm.d.ts");
	const dts = readFileSync(dtsPath, "utf8");
	if (!dts.includes("export default function init()")) {
		writeFileSync(
			dtsPath,
			`${dts}\nexport default function init(): Promise<void>;\n`,
		);
	}
}
