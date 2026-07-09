#!/usr/bin/env bun

import { buildLiveCollectionSubject } from "./switchable-baseline/subject";
import { REPOSITORY_ROOT } from "./switchable-baseline/model";
import { readPinnedFile } from "./switchable-baseline/gate-evidence/filesystem";
import { parseJsonDocument } from "./switchable-baseline/json";

function value(name: string): string {
	const index = process.argv.indexOf(`--${name}`);
	const result = index >= 0 ? process.argv[index + 1] : undefined;
	if (!result || result.startsWith("--")) throw new Error(`missing --${name}`);
	return result;
}

if (import.meta.main) {
	const replicaProvenancePath = process.env.WADDLE_REPLICA_PROVENANCE_PATH;
	if (!replicaProvenancePath) throw new Error("missing WADDLE_REPLICA_PROVENANCE_PATH");
	const replicaProvenance = parseJsonDocument(
		readPinnedFile(replicaProvenancePath, "replica provenance").bytes.toString("utf8"),
		"replica provenance",
	);
	const releaseArtifactProvenancePath = process.env.WADDLE_RELEASE_ARTIFACT_PROVENANCE_PATH;
	if (!releaseArtifactProvenancePath) {
		throw new Error("missing WADDLE_RELEASE_ARTIFACT_PROVENANCE_PATH");
	}
	const releaseArtifactProvenance = parseJsonDocument(
		readPinnedFile(
			releaseArtifactProvenancePath,
			"release artifact provenance",
		).bytes.toString("utf8"),
		"release artifact provenance",
	);
	buildLiveCollectionSubject({
		repositoryRoot: REPOSITORY_ROOT,
		release: {
			serverCommit: value("server-commit"),
			webCommit: value("web-commit"),
		},
		window: { start: value("start"), end: value("end") },
		deploymentScope: {
			job: process.env.WADDLE_CAPABILITY_JOB,
			environment: process.env.WADDLE_CAPABILITY_ENVIRONMENT,
			cluster: process.env.WADDLE_CAPABILITY_CLUSTER,
			namespace: process.env.WADDLE_CAPABILITY_NAMESPACE,
			expectedReplicas: Number(process.env.WADDLE_CAPABILITY_EXPECTED_REPLICAS),
			identityMetric: "waddle_build_info",
			targetSignalId: "server-deployment-identity-targets",
			identityLookbackSeconds: 3600,
		},
		replicaProvenance,
		releaseArtifactProvenance,
		environment: process.env,
	}).then(({ path }) => console.log(path)).catch((error: unknown) => {
		console.error(error instanceof Error ? error.message : "subject build failed");
		process.exitCode = 1;
	});
}
