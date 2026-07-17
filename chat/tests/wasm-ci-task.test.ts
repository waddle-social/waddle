import { describe, expect, test } from "bun:test";
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { renderTypedWasmDeclarations } from "../scripts/wasm-package-bindings.mjs";
import {
	WASM_AUTHENTICATION_CONDITIONS,
	WASM_DRIVER_ERROR_REASONS,
	WASM_STREAM_ERROR_CONDITIONS,
} from "../src/lib/xmpp/wasm-types";

const chatRoot = resolve(import.meta.dir, "..");
const envSource = readFileSync(resolve(chatRoot, "env.cue"), "utf8");
const generatedDeclarations = readFileSync(
	resolve(
		chatRoot,
		"../server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.d.ts",
	),
	"utf8",
);
const packageJson = JSON.parse(
	readFileSync(resolve(chatRoot, "package.json"), "utf8"),
) as { scripts?: Record<string, string> };
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

function cueObjectBlock(marker: string): string {
	const start = envSource.indexOf(marker);
	if (start < 0) throw new Error(`missing CUE object ${marker}`);
	const bodyStart = envSource.indexOf("{", start);
	if (bodyStart < 0) throw new Error(`missing CUE object body ${marker}`);
	let depth = 0;
	for (let index = bodyStart; index < envSource.length; index += 1) {
		const character = envSource[index];
		if (character === "{") depth += 1;
		if (character !== "}") continue;
		depth -= 1;
		if (depth === 0) return envSource.slice(start, index + 1);
	}
	throw new Error(`unterminated CUE object ${marker}`);
}

function taskBlock(name: string): string {
	return cueObjectBlock(`\t\t${name}: schema.#Task & {`);
}

function pipelineTasks(name: string): string[] {
	const block = cueObjectBlock(`\t\t${name}: {`);
	const taskList = block.match(/"tasks":\s*\[([^\]]+)\]/u)?.[1];
	if (!taskList) throw new Error(`missing task list for pipeline ${name}`);
	return [...taskList.matchAll(/tasks\.([A-Za-z][A-Za-z0-9]*)/gu)].map(
		(match) => match[1],
	);
}

function declarationStringUnion(name: string): string[] {
	const marker = `export type ${name} =`;
	const start = generatedDeclarations.indexOf(marker);
	if (start < 0) throw new Error(`missing generated declaration ${name}`);
	const end = generatedDeclarations.indexOf(";", start);
	if (end < 0) throw new Error(`unterminated generated declaration ${name}`);
	return [
		...generatedDeclarations
			.slice(start + marker.length, end)
			.matchAll(/"([^"]+)"/gu),
	].map((match) => match[1]);
}

function interfaceDeclaration(name: string): string {
	const marker = `export interface ${name} {`;
	const start = generatedDeclarations.indexOf(marker);
	if (start < 0) throw new Error(`missing generated interface ${name}`);
	const end = generatedDeclarations.indexOf("\n}", start);
	if (end < 0) throw new Error(`unterminated generated interface ${name}`);
	return generatedDeclarations.slice(start, end + 2);
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
	});

	test("browser durability is mandatory immediately after Bun tests with no hidden dependency", () => {
		const pullRequestTasks = pipelineTasks("pullRequest");
		const browserTask = taskBlock("browserDurability");
		expect(pullRequestTasks).toEqual([
			"test",
			"browserDurability",
			"lint",
			"build",
			"checkStartupBuild",
			"tokensCheck",
		]);
		expect(pullRequestTasks.indexOf("browserDurability")).toBe(
			pullRequestTasks.indexOf("test") + 1,
		);
		expect(browserTask).toContain('command: "bun"');
		expect(browserTask).toContain('args: ["run", "test:browser:durability"]');
		expect(browserTask).toContain('"tests/browser/**"');
		expect(browserTask).toContain('"src/**"');
		expect(browserTask).not.toContain("dependsOn:");
		expect(packageJson.scripts?.["test:browser:durability"]).toBe(
			"playwright test --config tests/browser/playwright.config.ts",
		);
	});

	test("startup artifact verification is reachable only through the canonical build edge", () => {
		const testTask = taskBlock("test");
		const checkTask = taskBlock("checkStartupBuild");

		expect(testTask).toContain('command: "bun"');
		expect(testTask).toContain('args: ["test"]');
		expect(testTask).not.toContain("dependsOn:");
		expect(testTask).not.toContain('"dist/**"');

		expect(checkTask).toContain('command: "bun"');
		expect(checkTask.match(/dependsOn:\s*\[([^\]]+)\]/u)?.[1]?.trim()).toBe("build");
		expect(checkTask).toContain('args: ["run", "check:startup-build"]');
		expect(checkTask).toContain('"scripts/check-startup-build.ts"');
		expect(checkTask).toContain('"dist/**"');
		expect(packageJson.scripts?.["check:startup-build"]).toBe(
			"bun run scripts/check-startup-build.ts",
		);
	});

	test("checked-in message sends and callbacks expose strict generated declarations", () => {
		expect(generatedDeclarations).toContain(
			"send_chat_message(peer_jid: string, body: string, options: WaddleSendOptions): Promise<WaddleSendMessageOutcome>;",
		);
		expect(generatedDeclarations).toContain(
			"send_groupchat_message(room_jid: string, body: string, options: WaddleSendOptions): Promise<WaddleSendMessageOutcome>;",
		);
		expect(generatedDeclarations).toContain(
			"set_on_error(cb: (error: WaddleControlErrorPayload) => void): void;",
		);
		expect(generatedDeclarations).toContain(
			"set_on_session_lifecycle(cb: (event: WaddleSessionLifecycle) => void): void;",
		);
		expect(generatedDeclarations).toContain(
			"set_on_stream_management(cb: (event: WaddleStreamManagementTelemetry) => void): void;",
		);
		expect(generatedDeclarations).toContain(
			'readonly kind: "driver-error";',
		);
		expect(generatedDeclarations).toContain(
			'readonly kind: "stream-error";',
		);
		expect(generatedDeclarations.match(/set_on_[a-z_]+\(cb: Function\)/gu)).toBeNull();
		expect(
			generatedDeclarations.match(
				/send_(?:chat|groupchat)_message\([^\n]+Promise<any>/gu,
			),
		).toBeNull();
	});

	test("checked-in resume state is one exact typed POD surface", () => {
		expect(generatedDeclarations).toContain(
			"get_resume_state(): WaddleResumeStateSnapshot | null;",
		);
		expect(generatedDeclarations).toContain(
			"with_resume_state(state: WaddleResumeStateSnapshot): void;",
		);
		const state = interfaceDeclaration("WaddleResumeStateSnapshot");
		expect(state).toContain("readonly previd: string;");
		expect(state).toContain("readonly inboundH: number;");
		expect(state).toContain("readonly outboundH: number;");
		expect(state).toContain(
			"readonly unhandledOutboundEntries: WaddleResumeEntrySnapshot[];",
		);
		expect(state).toContain("readonly maxResumeSeconds?: number;");
		expect(state).not.toContain("resource");
		expect(state).not.toContain("hasUnackedOutbound");
		expect(generatedDeclarations).not.toContain("get_resume_state(): any;");
		expect(generatedDeclarations).not.toContain("with_resume_state(state: any): void;");
		for (const removed of [
			"get_resume_state_handle(",
			"with_resume_state_entries(",
			"with_resume_state_entries_with_max(",
			"with_resume_state_handle(",
			"with_resume_state_with_max(",
			"export class WaddleResumeState",
		]) {
			expect(generatedDeclarations).not.toContain(removed);
		}
	});

	test("generated control-error unions cannot drift from the application boundary", () => {
		expect(declarationStringUnion("WaddleDriverErrorReason")).toEqual([
			...WASM_DRIVER_ERROR_REASONS,
		]);
		expect(declarationStringUnion("WaddleAuthenticationCondition")).toEqual([
			...WASM_AUTHENTICATION_CONDITIONS,
		]);
		expect(declarationStringUnion("WaddleStreamErrorCondition")).toEqual([
			...WASM_STREAM_ERROR_CONDITIONS,
		]);
	});

	test("the package finalizer upgrades raw signatures idempotently and fails closed on drift", () => {
		const rawGet = "    get_resume_state(): any;";
		const typedGet =
			"    get_resume_state(): WaddleResumeStateSnapshot | null;";
		const rawWith = "    with_resume_state(state: any): void;";
		const typedWith =
			"    with_resume_state(state: WaddleResumeStateSnapshot): void;";
		const rawSend =
			"    send_chat_message(peer_jid: string, body: string, options: any): Promise<any>;";
		const typedSend =
			"    send_chat_message(peer_jid: string, body: string, options: WaddleSendOptions): Promise<WaddleSendMessageOutcome>;";
		const rawError = "    set_on_error(cb: Function): void;";
		const typedError =
			"    set_on_error(cb: (error: WaddleControlErrorPayload) => void): void;";
		const partiallyRaw = generatedDeclarations
			.replace(typedGet, rawGet)
			.replace(typedWith, rawWith)
			.replace(typedSend, rawSend)
			.replace(typedError, rawError);

		expect(renderTypedWasmDeclarations(partiallyRaw)).toBe(
			generatedDeclarations,
		);
		expect(renderTypedWasmDeclarations(generatedDeclarations)).toBe(
			generatedDeclarations,
		);

		const drifted = generatedDeclarations.replace(
			typedError,
			"    set_on_control_error(cb: Function): void;",
		);
		expect(() => renderTypedWasmDeclarations(drifted)).toThrow(
			"WASM declaration drift",
		);

		const missingResumeType = generatedDeclarations.replace(
			"export interface WaddleResumeEntrySnapshot {",
			"export interface MissingResumeEntrySnapshot {",
		);
		expect(() => renderTypedWasmDeclarations(missingResumeType)).toThrow(
			"missing typed resume declaration",
		);

		const legacyResume = generatedDeclarations.replace(
			"export class WaddleClient",
			"export class WaddleResumeState {}\nexport class WaddleClient",
		);
		expect(() => renderTypedWasmDeclarations(legacyResume)).toThrow(
			"legacy resume surface",
		);

		const legacyBoolean = generatedDeclarations.replace(
			"    readonly unhandledOutboundEntries: WaddleResumeEntrySnapshot[];",
			"    readonly hasUnackedOutbound: boolean;\n    readonly unhandledOutboundEntries: WaddleResumeEntrySnapshot[];",
		);
		expect(() => renderTypedWasmDeclarations(legacyBoolean)).toThrow(
			"legacy resume surface",
		);
	});
});
