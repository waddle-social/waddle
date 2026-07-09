#!/usr/bin/env bun
import { resolve } from "node:path";
import { parseBaselineCatalog } from "./catalog";
import { readCatalogAtCommit } from "./collector";
import { normalizeFaroAggregateExport } from "./faro";
import {
	ensureSafeOutputParent,
	resolveRestrictedFaroInput,
} from "./filesystem";
import { readPinnedFile } from "./gate-evidence/filesystem";
import { parseJsonDocument } from "./json";
import {
	CATALOG_PATH,
	REPOSITORY_ROOT,
	SWITCHABLE_BASELINE_INPUT_DIRECTORY,
} from "./model";
import { commitFilesNoClobber } from "./no-clobber";

const scopePattern = /^[a-z0-9](?:[a-z0-9._-]{0,62}[a-z0-9])?$/;
const commitPattern = /^[0-9a-f]{40}$/;
const instantPattern = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$/;

function parseArguments(values: string[]): Map<string, string> {
	const required = new Set([
		"input", "output", "signal-id", "server-commit", "web-commit",
		"deployment-environment", "cluster", "namespace", "start", "end",
	]);
	const options = new Map<string, string>();
	for (let index = 0; index < values.length; index += 2) {
		const option = values[index];
		const value = values[index + 1];
		if (!option?.startsWith("--") || value === undefined || value.startsWith("--")) {
			throw new Error("Faro normalizer arguments must be --name value pairs");
		}
		const name = option.slice(2);
		if (!required.has(name)) throw new Error(`unknown Faro normalizer option --${name}`);
		if (options.has(name)) throw new Error(`duplicate Faro normalizer option --${name}`);
		options.set(name, value);
	}
	for (const name of required) {
		if (!options.get(name)) throw new Error(`missing required Faro normalizer option --${name}`);
	}
	return options;
}

function requireScope(value: string, label: string): string {
	if (value === "unknown" || !scopePattern.test(value)) {
		throw new Error(`${label} must be a bounded lowercase deployment label`);
	}
	return value;
}

function requireInstant(value: string, label: string): string {
	if (!instantPattern.test(value) || !Number.isFinite(Date.parse(value))) {
		throw new Error(`${label} must be an RFC 3339 UTC instant`);
	}
	return value;
}

export async function runFaroNormalizer(argumentsList: string[]): Promise<string> {
	const options = parseArguments(argumentsList);
	const serverCommit = options.get("server-commit") as string;
	const webCommit = options.get("web-commit") as string;
	if (!commitPattern.test(serverCommit)) {
		throw new Error("--server-commit must be a full lowercase Git SHA");
	}
	if (!commitPattern.test(webCommit)) {
		throw new Error("--web-commit must be a full lowercase Git SHA");
	}
	const start = requireInstant(options.get("start") as string, "--start");
	const end = requireInstant(options.get("end") as string, "--end");
	if (Date.parse(end) <= Date.parse(start)) throw new Error("--end must be later than --start");
	const deploymentEnvironment = requireScope(
		options.get("deployment-environment") as string,
		"--deployment-environment",
	);
	const cluster = requireScope(options.get("cluster") as string, "--cluster");
	const namespace = requireScope(options.get("namespace") as string, "--namespace");

	const rawCatalog = readPinnedFile(CATALOG_PATH, "baseline signal catalog").bytes.toString("utf8");
	const committedCatalog = await readCatalogAtCommit(serverCommit);
	if (rawCatalog !== committedCatalog) {
		throw new Error("baseline catalog bytes do not match the asserted server commit");
	}
	const catalog = parseBaselineCatalog(parseJsonDocument(rawCatalog, "baseline signal catalog"));
	const signalId = options.get("signal-id") as string;
	const signal = catalog.signals.find(({ id }) => id === signalId);
	if (!signal || signal.source !== "faro" || signal.collection !== "manual-export") {
		throw new Error("--signal-id must identify a catalogued Faro signal");
	}

	const evidenceRoot = resolve(REPOSITORY_ROOT, "docs/evidence");
	const inputPath = await resolveRestrictedFaroInput(
		options.get("input") as string,
		evidenceRoot,
	);
	const expectedOutput = resolve(
		SWITCHABLE_BASELINE_INPUT_DIRECTORY,
		"faro",
		`${signalId}.json`,
	);
	const outputPath = resolve(options.get("output") as string);
	if (outputPath !== expectedOutput) {
		throw new Error(`--output must be ${expectedOutput}`);
	}
	const rawExport = readPinnedFile(inputPath, "restricted Faro aggregate input").bytes.toString("utf8");
	const normalized = normalizeFaroAggregateExport(rawExport, signal, {
		webCommit,
		deploymentEnvironment,
		cluster,
		namespace,
		window: { start, end },
	});
	const query = normalized.source.query;
	const artifact = {
		schemaVersion: 1,
		evidenceKind: "gate-0-faro-aggregate",
		role: `faro-${signalId}`,
		signalId,
		release: { serverCommit, webCommit },
		window: { start, end },
		scope: {
			sourceId: normalized.source.sourceId,
			deploymentEnvironment: query.deploymentEnvironment,
			release: query.release,
			cluster: query.cluster,
			namespace: query.namespace,
		},
		...normalized,
	};
	await ensureSafeOutputParent(REPOSITORY_ROOT, outputPath);
	await commitFilesNoClobber(
		REPOSITORY_ROOT,
		[{ path: outputPath, contents: `${JSON.stringify(artifact, null, 2)}\n` }],
		async () => undefined,
	);
	return outputPath;
}

if (import.meta.main) {
	try {
		const output = await runFaroNormalizer(process.argv.slice(2));
		console.log(output);
	} catch (error) {
		console.error(error instanceof Error ? error.message : "Faro normalization failed");
		process.exitCode = 1;
	}
}
