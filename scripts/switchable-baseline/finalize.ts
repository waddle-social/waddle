import { resolve } from "node:path";
import {
	requireSharedGateZeroRelease,
	validateArtifactManifestReference,
	type ArtifactManifestReference,
	type ValidatedGateZeroArtifactManifest,
} from "./gate-evidence";
import {
	canonicalManifestPaths,
	canonicalGateZeroPaths,
	type GateZeroArtifactEvidenceKind,
} from "./gate-evidence/common";
import {
	fileSha256,
	readPinnedFile,
	requirePinnedFileUnchanged,
	type RepositorySourceAtCommitReader,
} from "./gate-evidence/filesystem";
import { verifyAttestedGateZeroPackage } from "./attested-package";
import {
	LIVE_COLLECTION_BUNDLE_PATH,
	LIVE_COLLECTION_SUBJECT_PATH,
	type LiveCollectionAttestationVerifier,
} from "./attestation";
import type { ReleaseArtifactProvenanceVerifier } from "./release-artifact-provenance";
export type {
	FinalizationContext,
	FinalizationMetadata,
} from "./finalize/common";

function manifestReference(
	repositoryRoot: string,
	kind: GateZeroArtifactEvidenceKind,
	sha256 = fileSha256(resolve(repositoryRoot, canonicalManifestPaths[kind])),
): ArtifactManifestReference {
	const path = canonicalManifestPaths[kind];
	return {
		type: "artifact-manifest",
		path,
		sha256,
	};
}

export async function verifyGateZeroEvidencePackage(
	repositoryRoot: string,
	sourceAtCommit?: RepositorySourceAtCommitReader,
	attestationVerifier?: LiveCollectionAttestationVerifier,
	releaseArtifactVerifier?: ReleaseArtifactProvenanceVerifier,
): Promise<ValidatedGateZeroArtifactManifest[]> {
	const packagePaths = [
		...canonicalGateZeroPaths,
		LIVE_COLLECTION_SUBJECT_PATH,
		LIVE_COLLECTION_BUNDLE_PATH,
	];
	const packageSnapshots = new Map(packagePaths.map((path) => [
		path,
		readPinnedFile(resolve(repositoryRoot, path), `Gate 0 package ${path}`),
	]));
	const manifests = await Promise.all(
		(["capability-baseline", "telemetry-baseline"] as const).map((kind) =>
			validateArtifactManifestReference(
				repositoryRoot,
				manifestReference(
					repositoryRoot,
					kind,
					packageSnapshots.get(canonicalManifestPaths[kind])?.sha256,
				),
				kind,
				sourceAtCommit,
			)
		),
	);
	requireSharedGateZeroRelease(manifests);
	await verifyAttestedGateZeroPackage(
		repositoryRoot,
		manifests,
		attestationVerifier,
		releaseArtifactVerifier,
	);
	for (const [path, snapshot] of packageSnapshots) {
		requirePinnedFileUnchanged(
			resolve(repositoryRoot, path),
			snapshot,
			`Gate 0 package ${path}`,
		);
	}
	return manifests;
}
