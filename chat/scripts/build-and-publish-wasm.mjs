/**
 * CI build-and-publish script for @waddle/xmpp-client-wasm.
 *
 * Runs in the CI `publishWasm` pipeline on every merge to main.
 * Requires: wasm-pack, wasm32-unknown-unknown Rust target, NODE_AUTH_TOKEN env var.
 *
 * Usage: bun run scripts/build-and-publish-wasm.mjs
 */
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..", "..");
const crateDir = resolve(repoRoot, "server/crates/waddle-xmpp-client-wasm");
const outDir = resolve(repoRoot, "server/wasm-pkg/waddle-xmpp-client-wasm");

if (!process.env.NODE_AUTH_TOKEN) {
  console.error("[wasm] NODE_AUTH_TOKEN is required to publish to GitHub Packages.");
  process.exit(1);
}

// Build
console.log("[wasm] Building @waddle/xmpp-client-wasm from Rust source...");
execFileSync(
  "wasm-pack",
  ["build", "--target", "bundler", "--out-dir", "../../wasm-pkg/waddle-xmpp-client-wasm"],
  { cwd: crateDir, stdio: "inherit" },
);

// Fix up package.json (wasm-pack overwrites it with the crate name).
const pkgJsonPath = resolve(outDir, "package.json");
const pkg = JSON.parse(readFileSync(pkgJsonPath, "utf8"));
pkg.name = "@waddle/xmpp-client-wasm";
pkg.publishConfig = { registry: "https://npm.pkg.github.com", access: "public" };
writeFileSync(pkgJsonPath, `${JSON.stringify(pkg, null, 2)}\n`);

// Replace the wasm-pack bundler entry point with a Vite-compatible one that
// passes the correct import object so WebAssembly.instantiate() receives the
// JS glue functions the WASM binary imports from "./waddle_xmpp_client_wasm_bg.js".
const jsPath = resolve(outDir, "waddle_xmpp_client_wasm.js");
writeFileSync(
  jsPath,
  `/* @ts-self-types="./waddle_xmpp_client_wasm.d.ts" */
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
`,
);

// Patch the .d.ts to include the default init export.
const dtsPath = resolve(outDir, "waddle_xmpp_client_wasm.d.ts");
const dts = readFileSync(dtsPath, "utf8");
if (!dts.includes("export default function init()")) {
  writeFileSync(dtsPath, `${dts}\nexport default function init(): Promise<void>;\n`);
}

// Publish to GitHub Packages.
console.log("[wasm] Publishing @waddle/xmpp-client-wasm to GitHub Packages...");
execFileSync("bun", ["publish", "--access", "public"], {
  cwd: outDir,
  stdio: "inherit",
  env: { ...process.env },
});

console.log("[wasm] Published successfully.");
