import { createHash } from "node:crypto";
import { selectAutomatedPrometheusSignals } from "./catalog";
import {
	type BaselineCatalog,
	compareText,
	type CollectedPrometheusSignal,
	type CollectionRequest,
	JSON_EVIDENCE_FILENAME,
	QUERY_STEP_SECONDS,
	type SwitchableBaselineEvidence,
} from "./model";

export const MANUAL_FARO_NOTE =
	"Faro exports must be collected and reviewed separately; Prometheus evidence does not prove browser journey outcomes or Gate 0 readiness.";
export const PARTIAL_EVIDENCE_CONCLUSION =
	"This artifact is partial Gate 0 evidence. It does not mark Gate 0 or the switchable-alternative milestone complete.";

export function sha256Hex(value: string | Uint8Array): string {
	return createHash("sha256").update(value).digest("hex");
}

export function buildSwitchableBaselineEvidence(
	catalog: BaselineCatalog,
	catalogSha256: string,
	request: CollectionRequest,
	collectedSignals: CollectedPrometheusSignal[],
): SwitchableBaselineEvidence {
	const expectedIds = selectAutomatedPrometheusSignals(catalog).map(
		({ id }) => id,
	);
	const actualSignals = [...collectedSignals].sort((left, right) =>
		compareText(left.id, right.id),
	);
	const actualIds = actualSignals.map(({ id }) => id);
	if (JSON.stringify(actualIds) !== JSON.stringify(expectedIds)) {
		throw new Error("collected Prometheus signals do not match the catalog");
	}

	const manualFaroSignalIds = catalog.signals
		.filter(
			({ source, collection }) =>
				source === "faro" && collection === "manual-export",
		)
		.map(({ id }) => id)
		.sort(compareText);
	if (manualFaroSignalIds.length === 0) {
		throw new Error("catalog defines no manual Faro signals");
	}

	return {
		schemaVersion: 1,
		evidenceKind: "gate-0-switchable-baseline",
		artifactRole: "prometheus-baseline",
		milestone: catalog.milestone,
		gate: 0,
		status: "partial",
		gateReadiness: "not-ready",
		serverCommit: request.serverCommit,
		deploymentScope: {
			job: request.job,
			environment: request.environment,
			cluster: request.cluster,
			namespace: request.namespace,
			expectedReplicas: request.expectedReplicas,
			identityMetric: catalog.deploymentScope.identityMetric,
			targetSignalId: catalog.deploymentScope.targetSignalId,
			identityLookbackSeconds:
				catalog.deploymentScope.maximumRangeLookbackSeconds,
		},
		catalog: {
			path: "docs/observability/switchable-baseline-signals.json",
			sha256: catalogSha256,
			schemaVersion: catalog.schemaVersion,
		},
		collectionWindow: {
			start: request.start,
			end: request.end,
			durationMinutes: request.durationMinutes,
			minimumDurationMinutes: catalog.minimumCollectionWindowMinutes,
			stepSeconds: QUERY_STEP_SECONDS,
		},
		automatedPrometheus: {
			status: "collected",
			signals: actualSignals,
		},
		manualFaro: {
			status: "required",
			signalIds: manualFaroSignalIds,
			note: MANUAL_FARO_NOTE,
		},
		conclusion: PARTIAL_EVIDENCE_CONCLUSION,
	};
}

export function serializeEvidence(
	evidence: SwitchableBaselineEvidence,
): string {
	return `${JSON.stringify(evidence, null, 2)}\n`;
}

function escapeMarkdown(value: string): string {
	return value.replaceAll("|", "\\|").replaceAll("\n", " ");
}

export function renderEvidenceMarkdown(
	evidence: SwitchableBaselineEvidence,
	jsonSha256: string,
): string {
	const rows = evidence.automatedPrometheus.signals.map((signal) => {
		const sampleCount = signal.series.reduce(
			(total, series) => total + series.samples.length,
			0,
		);
		const endValues = signal.series.map((series) => {
			const dimensions = Object.entries(series.attributes)
				.map(([key, value]) => `${key}=${value}`)
				.join(",");
			const prefix = dimensions.length > 0 ? `${dimensions}:` : "";
			return `${prefix}${series.canonicalEndSample.value}`;
		});
		return `| \`${escapeMarkdown(signal.id)}\` | ${signal.series.length} | ${sampleCount} | \`${escapeMarkdown(endValues.join("; "))}\` | ${escapeMarkdown(signal.unit)} |`;
	});
	const faroSignals = evidence.manualFaro.signalIds.map((id) => `- \`${id}\``);

	return `# Gate 0 switchable baseline evidence

Status: **partial — Gate 0 remains not ready**

This report records the automated Prometheus portion of the Gate 0 baseline. It does not establish browser journey outcomes, send-to-visible latency, unique-human activity, or overall Gate 0 readiness.

## Provenance

- Server commit: \`${evidence.serverCommit}\`
- Prometheus job: \`${evidence.deploymentScope.job}\`
- Environment: \`${evidence.deploymentScope.environment}\`
- Cluster: \`${evidence.deploymentScope.cluster}\`
- Namespace: \`${evidence.deploymentScope.namespace}\`
- Expected replicas: ${evidence.deploymentScope.expectedReplicas}
- Deployment identity metric: \`${evidence.deploymentScope.identityMetric}\`
- Deployment identity pre-window: ${evidence.deploymentScope.identityLookbackSeconds} seconds
- Window: \`${evidence.collectionWindow.start}\` to \`${evidence.collectionWindow.end}\` (${evidence.collectionWindow.durationMinutes} minutes)
- Query step: ${evidence.collectionWindow.stepSeconds} seconds
- Catalog: \`${evidence.catalog.path}\`
- Catalog SHA-256: \`${evidence.catalog.sha256}\`
- \`${JSON_EVIDENCE_FILENAME}\` SHA-256: \`${jsonSha256}\`

## Automated Prometheus collection

| Signal | Series | Range samples | Canonical end value(s) | Unit |
| --- | ---: | ---: | --- | --- |
${rows.join("\n")}

Every catalogued automated query returned the exact declared series combinations and a finite sample at every query-step timestamp. The deployment identity target exposed the complete revision set in scope and equalled the expected replica count from the start of the maximum range lookback through the evidence window. Each \`canonicalEndSample\` is the frozen-window result at \`${evidence.collectionWindow.end}\`.

Query results retain the catalog's stated interpretations and limitations in the JSON artifact; this collector does not turn those operational signals into stronger delivery or visibility claims.

## Manual Faro evidence still required

${faroSignals.join("\n")}

${evidence.manualFaro.note}

## Conclusion

${evidence.conclusion}
`;
}
