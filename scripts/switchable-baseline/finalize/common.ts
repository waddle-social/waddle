import {
	parseDeploymentScope,
	parseRelease,
	parseWindow,
	requireUtcInstant,
	type CapabilityArtifact,
	type ArtifactManifestReference,
	type EvidenceDeploymentScope,
	type EvidenceRelease,
	type EvidenceWindow,
	type TelemetryArtifact,
} from "../gate-evidence/common";
import {
	readPinnedFile,
	type RepositorySourceAtCommitReader,
} from "../gate-evidence/filesystem";
import { sha256Hex } from "../evidence";
import { resolveRestrictedEvidenceInput } from "../filesystem";
import { parseJsonDocument } from "../json";

export interface FinalizationMetadata {
	release: EvidenceRelease;
	window: EvidenceWindow;
	capturedAt: string;
	deploymentScope: EvidenceDeploymentScope;
}

export interface FinalizationContext extends FinalizationMetadata {
	repositoryRoot: string;
	sourceAtCommit?: RepositorySourceAtCommitReader;
}

export interface GeneratedEvidenceFile {
	repositoryPath: string;
	contents: string | Uint8Array;
}

export interface GeneratedEvidence {
	reference: ArtifactManifestReference;
	files: GeneratedEvidenceFile[];
}

export function validateFinalizationMetadata(
	value: FinalizationMetadata,
): FinalizationMetadata {
	const release = parseRelease(value.release, "finalizer.release");
	const window = parseWindow(value.window, "finalizer.window");
	if (Date.parse(window.end) - Date.parse(window.start) < 60 * 60 * 1_000) {
		throw new Error("evidence finalizer window must span at least 60 minutes");
	}
	const capturedAt = requireUtcInstant(value.capturedAt, "finalizer.capturedAt");
	if (Date.parse(capturedAt) < Date.parse(window.end)) {
		throw new Error("evidence finalizer capturedAt must not precede the window end");
	}
	const deploymentScope = parseDeploymentScope(
		value.deploymentScope,
		"finalizer.deploymentScope",
	);
	return { release, window, capturedAt, deploymentScope };
}

export function serializeCanonicalJson(value: unknown): string {
	return `${JSON.stringify(value, null, 2)}\n`;
}

export async function readRestrictedJsonInput(
	path: string,
	repositoryRoot: string,
	label: string,
): Promise<{ value: unknown; contents: string; sha256: string }> {
	const input = await resolveRestrictedEvidenceInput(
		path,
		repositoryRoot,
		label,
	);
	const raw = readPinnedFile(input, label).bytes.toString("utf8");
	const value = parseJsonDocument(raw, label);
	const contents = serializeCanonicalJson(value);
	return { value, contents, sha256: sha256Hex(contents) };
}

export function capabilityArtifact(
	metadata: FinalizationMetadata,
	role: CapabilityArtifact["role"],
	path: string,
	sha256: string,
): CapabilityArtifact {
	return { role, path, sha256, release: metadata.release, window: metadata.window };
}

export function telemetryArtifact(
	metadata: FinalizationMetadata,
	role: TelemetryArtifact["role"],
	path: string,
	sha256: string,
): TelemetryArtifact {
	return { role, path, sha256, release: metadata.release, window: metadata.window };
}
