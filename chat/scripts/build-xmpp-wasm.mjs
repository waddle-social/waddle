import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..", "..");
const crateDir = resolve(repoRoot, "server/crates/waddle-xmpp-client-wasm");
const outDir = resolve(repoRoot, "server/wasm-pkg/waddle-xmpp-client-wasm");
const nodeModulesDir = resolve(scriptDir, "..", "node_modules/@waddle/xmpp-client-wasm");

if (process.env.REBUILD_WASM !== "1") {
  // If _bg.js doesn't exist in node_modules, the package is incomplete (fresh checkout or
  // bun install after the artifact was removed). Auto-rebuild to avoid a broken dev server.
  const bgJsPath = resolve(nodeModulesDir, "waddle_xmpp_client_wasm_bg.js");
  const realBgJsPath = existsSync(nodeModulesDir)
    ? resolve(realpathSync(nodeModulesDir), "waddle_xmpp_client_wasm_bg.js")
    : bgJsPath;
  if (!existsSync(realBgJsPath)) {
    console.log("[wasm] WASM artifacts missing — auto-rebuilding from Rust source...");
    console.log("[wasm] (Set REBUILD_WASM=1 explicitly to force a rebuild when artifacts exist.)");
    // Fall through to the rebuild path below.
  } else {
    // Artifacts present; rely on the local build or published registry version.
    console.log("[wasm] WASM artifacts found — skipping rebuild. Set REBUILD_WASM=1 to force recompile.");
    process.exit(0);
  }
} else {
  console.log("[wasm] REBUILD_WASM=1 — building @waddle/xmpp-client-wasm from Rust source...");
}

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

// Copy compiled artifacts into node_modules so the local build uses this version.
// We write each file individually (writeFileSync, not cpSync) so that bun's
// hardlinked package cache is updated in-place: bun hardlinks the same inode
// for both the file: path and its internal .bun/ cache directory. cpSync would
// create new inodes (breaking hardlinks and leaving the bun cache stale), while
// writeFileSync writes through the existing inode so every hardlinked copy sees
// the new content immediately.
console.log("[wasm] Installing local build into node_modules...");
const realNodeModulesDir = existsSync(nodeModulesDir)
  ? realpathSync(nodeModulesDir)
  : nodeModulesDir;
mkdirSync(realNodeModulesDir, { recursive: true });
function copyDirInPlace(src, dest) {
  mkdirSync(dest, { recursive: true });
  for (const entry of readdirSync(src)) {
    const srcPath = resolve(src, entry);
    const destPath = resolve(dest, entry);
    if (statSync(srcPath).isDirectory()) {
      copyDirInPlace(srcPath, destPath);
    } else {
      writeFileSync(destPath, readFileSync(srcPath));
    }
  }
}
copyDirInPlace(outDir, realNodeModulesDir);

// Clear Vite's module transform cache so it doesn't serve stale _bg.js from a previous build.
// Without this, a mismatch between the cached glue JS and the newly compiled .wasm causes
// runtime errors like "wasm.__wasm_bindgen_func_elem_N is not a function".
const viteCacheDir = resolve(scriptDir, "..", "node_modules", ".vite");
if (existsSync(viteCacheDir)) {
  rmSync(viteCacheDir, { recursive: true, force: true });
  console.log("[wasm] Cleared Vite module cache.");
}

console.log("[wasm] Done. Local build installed.");
console.log("[wasm] ⚠️  Next `bun install` will revert to the published registry version.");

