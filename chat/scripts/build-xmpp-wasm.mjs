import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..", "..");
const crateDir = resolve(repoRoot, "server/crates/waddle-xmpp-client-wasm");
const outDir = resolve(repoRoot, "server/wasm-pkg/waddle-xmpp-client-wasm");
const pkgJsonPath = resolve(outDir, "package.json");
const jsPath = resolve(outDir, "waddle_xmpp_client_wasm.js");
const dtsPath = resolve(outDir, "waddle_xmpp_client_wasm.d.ts");

execFileSync(
  "wasm-pack",
  ["build", "--target", "bundler", "--out-dir", "../../wasm-pkg/waddle-xmpp-client-wasm"],
  { cwd: crateDir, stdio: "inherit" },
);

const pkg = JSON.parse(readFileSync(pkgJsonPath, "utf8"));
pkg.name = "@waddle/xmpp-client-wasm";
writeFileSync(pkgJsonPath, `${JSON.stringify(pkg, null, 2)}\n`);

writeFileSync(
  jsPath,
  `/* @ts-self-types="./waddle_xmpp_client_wasm.d.ts" */\nimport initWasm from "./waddle_xmpp_client_wasm_bg.wasm?init";\nimport { __wbg_set_wasm } from "./waddle_xmpp_client_wasm_bg.js";\n\nlet initPromise;\n\nexport default async function init() {\n  if (!initPromise) {\n    initPromise = initWasm().then((wasm) => {\n      __wbg_set_wasm(wasm);\n    });\n  }\n  return initPromise;\n}\n\nexport { WaddleClient, WaddleConfig } from "./waddle_xmpp_client_wasm_bg.js";\n`,
);

const dts = readFileSync(dtsPath, "utf8");
if (!dts.includes("export default function init()")) {
  writeFileSync(dtsPath, `${dts}\nexport default function init(): Promise<void>;\n`);
}
