import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readdirSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..", "..");
const crateDir = resolve(repoRoot, "server/crates/waddle-xmpp-client-wasm");
const clientCrateDir = resolve(repoRoot, "server/crates/waddle-xmpp-client");
const coreCrateDir = resolve(repoRoot, "server/crates/waddle-xmpp-core");
const outDir = resolve(repoRoot, "server/wasm-pkg/waddle-xmpp-client-wasm");
const nodeModulesDir = resolve(scriptDir, "..", "node_modules/@waddle/xmpp-client-wasm");

// Content hash of all Rust sources feeding the wasm build. Used instead of
// file mtimes for staleness: a fresh `git checkout` rewrites every file's mtime
// to checkout time in a non-deterministic order, so an mtime comparison would
// randomly decide the stale checked-in artifact is "newer" than the sources and
// skip the rebuild. In CI that means linting/building against stale generated
// `.d.ts`, which flips knip's unused-export analysis pass↔fail. A content hash
// is stable across checkouts and only changes when the Rust source changes.
function rustSourceHash(paths) {
  const files = [];
  function visit(path) {
    const stat = statSync(path);
    if (stat.isDirectory()) {
      for (const entry of readdirSync(path)) {
        if (entry === "target" || entry === ".git") continue;
        visit(resolve(path, entry));
      }
      return;
    }
    if (path.endsWith(".rs") || path.endsWith(".toml") || path.endsWith(".lock")) {
      files.push(path);
    }
  }
  for (const path of paths) visit(path);
  files.sort();
  const hash = createHash("sha256");
  for (const file of files) {
    hash.update(file);
    hash.update("\0");
    hash.update(readFileSync(file));
  }
  return hash.digest("hex").slice(0, 16);
}

const SOURCE_HASH_FILE = "waddle_xmpp_client_wasm.srchash";
const rustSourcePaths = [crateDir, clientCrateDir, coreCrateDir];

function artifactBuildId(paths) {
  const hash = createHash("sha256");
  for (const path of paths) {
    hash.update(readFileSync(path));
  }
  return hash.digest("hex").slice(0, 12);
}

if (process.env.REBUILD_WASM !== "1") {
  // Rebuild when the installed artifact is missing/incomplete (fresh checkout or
  // a `bun install` that pulled the checked-in package), OR when the Rust source
  // content hash differs from the one recorded next to the installed artifact.
  // The hash sidecar lives only in node_modules (gitignored), so a fresh install
  // always has no sidecar and rebuilds once from current source — guaranteeing CI
  // never lints/builds against a stale generated `.d.ts`.
  const realDir = existsSync(nodeModulesDir) ? realpathSync(nodeModulesDir) : nodeModulesDir;
  const realBgJsPath = resolve(realDir, "waddle_xmpp_client_wasm_bg.js");
  const sidecarPath = resolve(realDir, SOURCE_HASH_FILE);
  const currentHash = rustSourceHash(rustSourcePaths);
  if (!existsSync(realBgJsPath)) {
    console.log("[wasm] WASM artifacts missing — auto-rebuilding from Rust source...");
    console.log("[wasm] (Set REBUILD_WASM=1 explicitly to force a rebuild when artifacts exist.)");
    // Fall through to the rebuild path below.
  } else if (!existsSync(sidecarPath) || readFileSync(sidecarPath, "utf8").trim() !== currentHash) {
    console.log("[wasm] Rust wasm sources changed (content hash mismatch) — rebuilding @waddle/xmpp-client-wasm...");
  } else {
    // Artifacts present and built from the current Rust source.
    console.log("[wasm] WASM artifacts up to date (source hash match) — skipping rebuild. Set REBUILD_WASM=1 to force.");
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
//
// We import the WASM binary as a URL (?url) and instantiate it manually so that
// in dev mode we can set cache: "no-store" on the fetch.  This prevents the
// browser from serving a stale compiled WebAssembly module after a rebuild,
// which would cause "wasm.__wasm_bindgen_func_elem_N is not a function" errors
// when the .wasm binary and _bg.js glue are from different builds.
// In production the ?url resolves to a content-hashed filename so the browser
// cache is correctly busted without needing no-store.
//
// We also embed a content-derived query string into the imports of _bg.js and
// the .wasm URL. Vite treats different query strings as different module IDs,
// which busts the dev server transform cache and the browser HTTP cache when
// the generated artifact actually changes. The ID is deterministic so linting
// or rebuilding unchanged Rust sources does not dirty the checked-in package.
const buildId = artifactBuildId([
  resolve(outDir, "waddle_xmpp_client_wasm_bg.js"),
  resolve(outDir, "waddle_xmpp_client_wasm_bg.wasm"),
]);
const jsPath = resolve(outDir, "waddle_xmpp_client_wasm.js");
writeFileSync(
  jsPath,
  `/* @ts-self-types="./waddle_xmpp_client_wasm.d.ts" */
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

// Record the Rust source hash this artifact was built from so subsequent runs
// can skip an unchanged rebuild deterministically (mtime-free). Written only to
// node_modules (gitignored) — never to the checked-in package — so a fresh
// install always rebuilds once rather than trusting a stale recorded hash.
writeFileSync(resolve(realNodeModulesDir, SOURCE_HASH_FILE), `${rustSourceHash(rustSourcePaths)}\n`);

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
