import { afterEach, describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, resolve } from "node:path";

const chatRoot = resolve(import.meta.dir, "..");
const packageJson = JSON.parse(
  readFileSync(resolve(chatRoot, "package.json"), "utf8"),
) as { scripts?: Record<string, string> };
const scripts = packageJson.scripts ?? {};
const buildScript = readFileSync(
  resolve(chatRoot, "scripts/build-xmpp-wasm.mjs"),
  "utf8",
);
const envSource = readFileSync(resolve(chatRoot, "env.cue"), "utf8");
let fixtureRoot: string | undefined;

afterEach(() => {
  if (fixtureRoot) rmSync(fixtureRoot, { recursive: true, force: true });
  fixtureRoot = undefined;
});

function writeFixture(path: string, contents: string) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, contents);
}

function cueList(name: string): string[] {
  const marker = `let ${name} = [`;
  const start = envSource.indexOf(marker);
  if (start < 0) throw new Error(`missing CUE list ${name}`);
  const bodyStart = start + marker.length;
  const end = envSource.indexOf("\n]", bodyStart);
  if (end < 0) throw new Error(`unterminated CUE list ${name}`);
  return [...envSource.slice(bodyStart, end).matchAll(/"([^"]+)"/gu)].map(
    (match) => match[1],
  );
}

function taskBlock(name: string): string {
  const marker = `\t\t${name}: schema.#Task & {`;
  const start = envSource.indexOf(marker);
  if (start < 0) throw new Error(`missing CUE task ${name}`);
  const bodyStart = envSource.indexOf("{", start);
  let depth = 0;
  for (let index = bodyStart; index < envSource.length; index += 1) {
    const character = envSource[index];
    if (character === "{") depth += 1;
    if (character !== "}") continue;
    depth -= 1;
    if (depth === 0) return envSource.slice(start, index + 1);
  }
  throw new Error(`unterminated CUE task ${name}`);
}

describe("UI WASM build contract", () => {
  test("every direct UI command routes through a fresh WASM build", () => {
    for (const name of ["dev", "build", "astro", "lint", "lint:fix"]) {
      expect(scripts[name], `${name} must rebuild WASM first`).toStartWith(
        "bun run wasm:build && ",
      );
    }
    expect(scripts.build).toContain("astro check && astro build");
    expect(scripts.preview).toBe("bun run build && astro preview");
    expect(scripts.deploy).toBe(
      "bun run build && bun run sourcemaps:strip && wrangler deploy",
    );
  });

  test("keeps the generated WASM package local to UI builds", () => {
    expect(buildScript).not.toContain("publishConfig");
    expect(envSource).not.toContain("publishWasm");
    expect(envSource).not.toContain("buildAndPublishWasm");
  });

  test("invokes wasm-pack even when an installed artifact has a newer mtime", () => {
    fixtureRoot = mkdtempSync(resolve(tmpdir(), "waddle-wasm-ui-build-"));
    const fixtureScript = resolve(
      fixtureRoot,
      "chat/scripts/build-xmpp-wasm.mjs",
    );
    writeFixture(fixtureScript, buildScript);

    const oldTime = new Date("2026-01-01T00:00:00Z");
    for (const crate of [
      "waddle-xmpp-client-wasm",
      "waddle-xmpp-client",
      "waddle-xmpp-core",
    ]) {
      const source = resolve(fixtureRoot, `server/crates/${crate}/src/lib.rs`);
      writeFixture(source, "pub fn fixture() {}\n");
      utimesSync(source, oldTime, oldTime);
    }

    const installedArtifact = resolve(
      fixtureRoot,
      "chat/node_modules/@waddle/xmpp-client-wasm/waddle_xmpp_client_wasm_bg.js",
    );
    writeFixture(installedArtifact, "// newer but stale\n");
    const newerTime = new Date("2026-02-01T00:00:00Z");
    utimesSync(installedArtifact, newerTime, newerTime);

    const sentinel = resolve(fixtureRoot, "wasm-pack-invoked");
    const stub = resolve(fixtureRoot, "bin/wasm-pack");
    writeFixture(stub, '#!/bin/sh\n: > "$WASM_PACK_SENTINEL"\nexit 73\n');
    chmodSync(stub, 0o755);

    const result = spawnSync(process.execPath, [fixtureScript], {
      encoding: "utf8",
      env: {
        ...process.env,
        PATH: [dirname(stub), process.env.PATH].filter(Boolean).join(delimiter),
        REBUILD_WASM: "0",
        WASM_PACK_SENTINEL: sentinel,
      },
    });

    expect(result.status).not.toBe(0);
    expect(existsSync(sentinel)).toBe(true);
  });

  test("cuenv invalidates UI roots for every WASM build input", () => {
    expect(cueList("_WasmSourceInputs")).toEqual([
      "../.cargo/**",
      "../flake.lock",
      "../flake.nix",
      "../server/.cargo/**",
      "../server/Cargo.lock",
      "../server/Cargo.toml",
      "../server/crates/waddle-xmpp-client-wasm/**",
      "../server/crates/waddle-xmpp-client/**",
      "../server/crates/waddle-xmpp-core/**",
      "../server/rust-toolchain.toml",
    ]);
    expect(envSource).toContain(
      "let _WasmBuildInputs = list.Concat([_WasmSourceInputs",
    );

    const buildWasm = taskBlock("buildWasm");
    expect(buildWasm).toContain('command: "bun"');
    expect(buildWasm).toContain('args: ["run", "wasm:build"]');
    expect(buildWasm).toContain("dependsOn: [wasmPipelineTrigger]");
    expect(buildWasm).toContain("inputs: _WasmBuildInputs");

    const pipelineTrigger = taskBlock("wasmPipelineTrigger");
    expect(pipelineTrigger).toContain('command: "true"');
    expect(pipelineTrigger).not.toContain("inputs:");

    const build = taskBlock("build");
    expect(build).toContain('command: "bash"');
    expect(build).toContain(
      "./node_modules/.bin/astro check && ./node_modules/.bin/astro build",
    );
    expect(build).not.toContain("wasm:build");
    expect(build).toContain("dependsOn: [buildWasm, generateTypes]");

    const lint = taskBlock("lint");
    expect(lint).toContain('args: ["run", "knip"]');
    expect(lint).not.toContain("wasm:build");
    expect(lint).toContain("dependsOn: [buildWasm, generateTypes]");
  });
});
