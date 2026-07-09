#!/usr/bin/env bun

import { buildJourneyBaselineManifest } from "./switchable-baseline/journey-baseline";
import { REPOSITORY_ROOT } from "./switchable-baseline/model";

function commitArgument(values: string[]): string {
	if (values.length !== 2 || values[0] !== "--commit" || !values[1]) {
		throw new Error("usage: build-switchable-journey-baseline.ts --commit <full-git-sha>");
	}
	return values[1];
}

if (import.meta.main) {
	try {
		const reference = await buildJourneyBaselineManifest(
			REPOSITORY_ROOT,
			commitArgument(process.argv.slice(2)),
		);
		console.log(JSON.stringify(reference));
	} catch (error) {
		console.error(error instanceof Error ? error.message : "journey-baseline generation failed");
		process.exitCode = 1;
	}
}
