import {
	type TelemetryArtifactManifest,
} from "../gate-evidence";
import {
	canonicalArtifactPaths,
	canonicalGateZeroReviewPath,
	canonicalManifestPaths,
	requiredRoles,
	scopesEqual,
	type TelemetryArtifactRole,
} from "../gate-evidence/common";
import { validateFaroArtifact } from "../gate-evidence/faro";
import {
	requireRepositorySourceAtCommit,
} from "../gate-evidence/filesystem";
import { validatePrometheusArtifact } from "../gate-evidence/prometheus";
import { renderEvidenceMarkdown, sha256Hex } from "../evidence";
import type { SwitchableBaselineEvidence } from "../model";
import {
	FARO_WEB_SOURCE_PATHS,
	TELEMETRY_SERVER_SOURCE_PATHS,
} from "../source-contract";
import {
	readRestrictedJsonInput,
	serializeCanonicalJson,
	telemetryArtifact,
	validateFinalizationMetadata,
	type FinalizationContext,
	type GeneratedEvidence,
} from "./common";

type FaroRole = Exclude<TelemetryArtifactRole, "prometheus-baseline">;

export interface TelemetryFinalizationInput extends FinalizationContext {
	prometheusPath: string;
	faroPaths: Record<FaroRole, string>;
}

export async function buildTelemetryEvidence(
	input: TelemetryFinalizationInput,
): Promise<GeneratedEvidence> {
	const metadata = validateFinalizationMetadata(input);
	for (const source of TELEMETRY_SERVER_SOURCE_PATHS) {
		await requireRepositorySourceAtCommit(
			input.repositoryRoot,
			metadata.release.serverCommit,
			source,
			"telemetry finalizer server source",
			input.sourceAtCommit,
		);
	}
	for (const source of FARO_WEB_SOURCE_PATHS) {
		await requireRepositorySourceAtCommit(
			input.repositoryRoot,
			metadata.release.webCommit,
			source,
			"telemetry finalizer web source",
			input.sourceAtCommit,
		);
	}

	const prometheusInput = await readRestrictedJsonInput(
		input.prometheusPath,
		input.repositoryRoot,
		"Prometheus baseline export",
	);
	const prometheusArtifact = telemetryArtifact(
		metadata,
		"prometheus-baseline",
		canonicalArtifactPaths["telemetry-baseline"]["prometheus-baseline"],
		prometheusInput.sha256,
	);
	const prometheus = validatePrometheusArtifact(
		input.repositoryRoot,
		prometheusInput.value,
		prometheusArtifact,
	);
	if (!scopesEqual(prometheus.scope, metadata.deploymentScope)) {
		throw new Error("telemetry finalizer deployment scope must match Prometheus");
	}

	const artifacts = [prometheusArtifact];
	const faroOutputs: Array<{ repositoryPath: string; contents: string }> = [];
	for (const role of requiredRoles["telemetry-baseline"].slice(1) as FaroRole[]) {
		const repositoryPath = canonicalArtifactPaths["telemetry-baseline"][role];
		const faroInput = await readRestrictedJsonInput(
			input.faroPaths[role],
			input.repositoryRoot,
			role,
		);
		const artifact = telemetryArtifact(
			metadata,
			role,
			repositoryPath,
			faroInput.sha256,
		);
		validateFaroArtifact(
			faroInput.value,
			artifact,
			prometheus.catalog,
			prometheus.scope,
		);
		artifacts.push(artifact);
		faroOutputs.push({
			repositoryPath,
			contents: faroInput.contents,
		});
	}

	const manifest: TelemetryArtifactManifest = {
		schemaVersion: 1,
		evidenceKind: "telemetry-baseline",
		status: "complete",
		release: metadata.release,
		window: metadata.window,
		capturedAt: metadata.capturedAt,
		artifacts,
	};
	const manifestContents = serializeCanonicalJson(manifest);
	const reference = {
		type: "artifact-manifest" as const,
		path: canonicalManifestPaths["telemetry-baseline"],
		sha256: sha256Hex(manifestContents),
	};
	const review = renderEvidenceMarkdown(
		prometheusInput.value as SwitchableBaselineEvidence,
		prometheusInput.sha256,
	);
	return {
		reference,
		files: [
			{
				repositoryPath: prometheusArtifact.path,
				contents: prometheusInput.contents,
			},
			{
				repositoryPath: canonicalGateZeroReviewPath,
				contents: review,
			},
			...faroOutputs,
			{
				repositoryPath: reference.path,
				contents: manifestContents,
			},
		],
	};
}
