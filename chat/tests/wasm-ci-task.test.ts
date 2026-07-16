import { describe, expect, test } from "bun:test";
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";

const chatRoot = resolve(import.meta.dir, "..");
const envSource = readFileSync(resolve(chatRoot, "env.cue"), "utf8");
const formerWasmScriptInputs = Object.freeze([
	"scripts/wasm-build-contract.json",
	"scripts/wasm-build-contract.mjs",
	"scripts/wasm-build-environment.mjs",
	"scripts/wasm-build-executor.mjs",
	"scripts/wasm-build-input-digest.mjs",
	"scripts/wasm-build-input-manifest.mjs",
	"scripts/wasm-build-inputs.mjs",
	"scripts/wasm-build-module-closure.mjs",
	"scripts/wasm-cargo-metadata-executor.mjs",
	"scripts/wasm-cargo-metadata.mjs",
	"scripts/wasm-package-bindings.mjs",
	"scripts/wasm-rust-source-closure.mjs",
]);

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

function inputPatternMatches(pattern: string, path: string): boolean {
	let expression = "^";
	for (let index = 0; index < pattern.length; index += 1) {
		const character = pattern[index];
		if (character === "*" && pattern[index + 1] === "*") {
			expression += ".*";
			index += 1;
		} else if (character === "*") {
			expression += "[^/]*";
		} else {
			expression += character.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
		}
	}
	return new RegExp(`${expression}$`, "u").test(path);
}

describe("WASM CI task graph", () => {
	test("the compact wasm script pattern covers every former and current script input", () => {
		const inputs = cueList("_WasmBuildInputs");
		expect(inputs.filter((input) => input.startsWith("scripts/wasm-"))).toEqual(
			["scripts/wasm-*"],
		);
		expect(inputs).toContain("scripts/build-and-publish-wasm.mjs");
		expect(inputs).toContain("scripts/build-xmpp-wasm.mjs");

		for (const path of formerWasmScriptInputs) {
			expect(
				inputs.some((pattern) => inputPatternMatches(pattern, path)),
				`${path} must still trigger the WASM CI task`,
			).toBe(true);
		}
		const currentWasmScripts = readdirSync(resolve(chatRoot, "scripts"))
			.filter((name) => name.startsWith("wasm-"))
			.map((name) => `scripts/${name}`);
		for (const path of currentWasmScripts) {
			expect(
				inputs.some((pattern) => inputPatternMatches(pattern, path)),
				`${path} must trigger the WASM CI task`,
			).toBe(true);
		}
	});

	test("compaction leaves every non-script build input unchanged", () => {
		expect(
			cueList("_WasmBuildInputs").filter(
				(input) => !input.startsWith("scripts/"),
			),
		).toEqual([
			"../.cargo/**",
			"../flake.lock",
			"../flake.nix",
			"../server/.cargo/**",
			"../server/Cargo.lock",
			"../server/Cargo.toml",
			"../server/crates/**",
			"../server/rust-toolchain.toml",
			"env.cue",
			"package.json",
		]);
	});

	test("the combined task tracks exactly the three committed binding paths", () => {
		expect(cueList("_WasmTrackedBindings")).toEqual([
			"../server/wasm-pkg/waddle-xmpp-client-wasm/package.json",
			"../server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.d.ts",
			"../server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.js",
		]);
	});

	test("drift check precedes rebuild inside one strict task with no standalone graph node", () => {
		const start = envSource.indexOf("\t\tbuildWasm: schema.#Task");
		const end = envSource.indexOf("\n\t\tgenerateTypes:", start);
		const task = envSource.slice(start, end);
		const check = task.indexOf("bun run scripts/build-xmpp-wasm.mjs --check");
		const rebuild = task.indexOf("REBUILD_WASM=1 bun run wasm:build");

		expect(start).toBeGreaterThanOrEqual(0);
		expect(end).toBeGreaterThan(start);
		expect(task).toContain('command: "bash"');
		expect(task).toContain("set -euo pipefail");
		expect(check).toBeGreaterThanOrEqual(0);
		expect(rebuild).toBeGreaterThan(check);
		expect(task).toContain(
			"inputs: list.Concat([_WasmBuildInputs, _WasmTrackedBindings])",
		);
		expect(task).not.toContain("dependsOn:");
		expect(envSource).not.toContain("checkWasmDrift");
		expect(envSource).toContain(
			'"tasks": [tasks.test, tasks.lint, tasks.build, tasks.tokensCheck]',
		);
	});
});
