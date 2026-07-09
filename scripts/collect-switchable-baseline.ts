#!/usr/bin/env bun

import {
	CATALOG_PATH,
	collectBaselineArtifacts,
	JSON_EVIDENCE_FILENAME,
	MARKDOWN_EVIDENCE_FILENAME,
	REPOSITORY_ROOT,
	writeEvidencePairAtomically,
} from "./switchable-baseline";
import { resolve } from "node:path";
import { ensureSafeOutputParent } from "./switchable-baseline/filesystem";

async function main(): Promise<void> {
	const rawCatalog = await Bun.file(CATALOG_PATH).text();
	const result = await collectBaselineArtifacts({
		rawCatalog,
		argumentsList: Bun.argv.slice(2),
		environment: process.env,
	});
	await Promise.all([
		ensureSafeOutputParent(
			REPOSITORY_ROOT,
			resolve(result.request.outputDirectory, JSON_EVIDENCE_FILENAME),
		),
		ensureSafeOutputParent(
			REPOSITORY_ROOT,
			resolve(result.request.outputDirectory, MARKDOWN_EVIDENCE_FILENAME),
		),
	]);
	const paths = await writeEvidencePairAtomically(
		result.request.outputDirectory,
		result.jsonEvidence,
		result.markdownEvidence,
	);

	console.log(`Wrote partial Gate 0 Prometheus evidence to ${paths.jsonPath}`);
	console.log(`Wrote evidence summary to ${paths.markdownPath}`);
	console.log(`${JSON_EVIDENCE_FILENAME} SHA-256: ${result.jsonSha256}`);
	console.log(
		"Manual Faro evidence is still required; Gate 0 remains not ready.",
	);
}

if (import.meta.main) {
	main().catch((error: unknown) => {
		const message =
			error instanceof Error ? error.message : "unknown collector failure";
		console.error(`collect-switchable-baseline: ${message}`);
		process.exitCode = 1;
	});
}
