import { resolve } from "node:path";

export const QUERY_STEP_SECONDS = 60;
export const SCOPE_QUERY_PLACEHOLDER = "{{scope}}";
export const IDENTITY_COMMIT_PLACEHOLDER = "{{commit}}";
export const IDENTITY_ATTRIBUTE_BINDINGS = [
	{
		attribute: "commit",
		placeholder: IDENTITY_COMMIT_PLACEHOLDER,
		requestKey: "serverCommit",
	},
	{
		attribute: "exported_cluster",
		placeholder: "{{cluster}}",
		requestKey: "cluster",
	},
	{
		attribute: "exported_deployment_environment",
		placeholder: "{{environment}}",
		requestKey: "environment",
	},
] as const;
export const JSON_EVIDENCE_FILENAME = "telemetry-baseline.json";
export const MARKDOWN_EVIDENCE_FILENAME = "telemetry-baseline.md";
export const GATE_ZERO_PROMETHEUS_SIGNAL_IDS = [
	"connection-registry-entries",
	"live-delivery-channel-outcomes",
	"loss-corruption-safety",
	"message-archive-attempts",
	"push-pipeline-outcomes",
	"room-registry-entries",
	"room-registry-sample-freshness",
	"server-deployment-identity-targets",
	"server-process-start-continuity",
	"xmpp-sasl1-terminal-attempts",
] as const;
export const GATE_ZERO_FARO_SIGNAL_IDS = [
	"browser-auth-bootstrap",
	"browser-message-ack-latency",
	"browser-reconnect-duration",
	"browser-session-lifecycle",
] as const;

export const REPOSITORY_ROOT = resolve(import.meta.dir, "../..");
export const CATALOG_PATH = resolve(
	REPOSITORY_ROOT,
	"docs/observability/switchable-baseline-signals.json",
);
export const SWITCHABLE_BASELINE_INPUT_DIRECTORY = resolve(
	REPOSITORY_ROOT,
	"target/switchable-baseline-inputs",
);
export const DEFAULT_OUTPUT_DIRECTORY = resolve(
	SWITCHABLE_BASELINE_INPUT_DIRECTORY,
	"prometheus",
);

export const DEPLOYMENT_SCOPE_BINDINGS = [
	{ label: "job", requestKey: "job" },
	{
		label: "deployment_environment",
		requestKey: "environment",
	},
	{ label: "cluster", requestKey: "cluster" },
	{ label: "namespace", requestKey: "namespace" },
] as const;

export type DeploymentScopeLabel =
	(typeof DEPLOYMENT_SCOPE_BINDINGS)[number]["label"];
export type QueryPlaceholder = typeof SCOPE_QUERY_PLACEHOLDER;

export interface DeploymentScope {
	identityMetric: string;
	targetSignalId: string;
	maximumRangeLookbackSeconds: number;
	requiredLabels: DeploymentScopeLabel[];
	queryPlaceholders: QueryPlaceholder[];
}

export interface PrivacyContract {
	forbiddenAttributeFragments: string[];
	maximumValuesPerAttribute: number;
	prohibitedPayloads: string[];
}

export interface RequiredActivity {
	attributes: Record<string, string>;
	minimumValue: number;
}

export interface FaroQueryDefinition {
	sourceId: "waddle-chat";
	signalNames: string[];
	groupBy: string[];
	aggregates: string[];
}

export interface FaroQueryPlan extends FaroQueryDefinition {
	schemaVersion: 1;
	engine: "grafana-faro-aggregate";
	deploymentEnvironment: string;
	cluster: string;
	namespace: string;
	release: string;
	window: {
		start: string;
		end: string;
	};
}

export interface BaselineSignal {
	id: string;
	owner: string;
	source: string;
	collection: string;
	kind: string;
	metricNames: string[];
	attributes: Record<string, string[]>;
	unit: string;
	query: string;
	window: string;
	collectionLookbackSeconds?: number;
	requiredStability?: "constant";
	minimumAllowedValue?: number;
	maximumAllowedValue?: number;
	interpretation: string;
	limitations: string;
	requiredActivity?: RequiredActivity[];
	faroQuery?: FaroQueryDefinition;
}

export interface BaselineCatalog {
	schemaVersion: number;
	milestone: string;
	minimumCollectionWindowMinutes: number;
	deploymentScope: DeploymentScope;
	privacy: PrivacyContract;
	signals: BaselineSignal[];
}

export interface CollectorArguments {
	start: string;
	end: string;
	serverCommit: string;
	job: string;
	environment: string;
	cluster: string;
	namespace: string;
	expectedReplicas: string;
	outputDirectory: string;
}

export interface CollectionRequest {
	start: string;
	end: string;
	startEpochSeconds: number;
	endEpochSeconds: number;
	durationMinutes: number;
	serverCommit: string;
	job: string;
	environment: string;
	cluster: string;
	namespace: string;
	identityStartEpochSeconds: number;
	expectedReplicas: number;
	outputDirectory: string;
}

export interface PrometheusConfiguration {
	baseUrl: string;
	username: string;
	apiKey: string;
}

export interface PrometheusQueryRangePayload {
	status?: unknown;
	data?: unknown;
	warnings?: unknown;
	infos?: unknown;
}

export interface EvidenceSample {
	timestamp: number;
	value: number;
}

export interface EvidenceSeries {
	attributes: Record<string, string>;
	samples: EvidenceSample[];
	canonicalEndSample: EvidenceSample;
}

export interface CollectedPrometheusSignal {
	id: string;
	query: string;
	window: string;
	unit: string;
	collectionLookbackSeconds?: number;
	requiredStability?: "constant";
	minimumAllowedValue?: number;
	maximumAllowedValue?: number;
	interpretation: string;
	limitations: string;
	series: EvidenceSeries[];
}

export interface SwitchableBaselineEvidence {
	schemaVersion: 1;
	evidenceKind: "gate-0-switchable-baseline";
	artifactRole: "prometheus-baseline";
	milestone: string;
	gate: 0;
	status: "partial";
	gateReadiness: "not-ready";
	serverCommit: string;
	deploymentScope: {
		job: string;
		environment: string;
		cluster: string;
		namespace: string;
		expectedReplicas: number;
		identityMetric: string;
		targetSignalId: string;
		identityLookbackSeconds: number;
	};
	catalog: {
		path: "docs/observability/switchable-baseline-signals.json";
		sha256: string;
		schemaVersion: number;
	};
	collectionWindow: {
		start: string;
		end: string;
		durationMinutes: number;
		minimumDurationMinutes: number;
		stepSeconds: number;
	};
	automatedPrometheus: {
		status: "collected";
		signals: CollectedPrometheusSignal[];
	};
	manualFaro: {
		status: "required";
		signalIds: string[];
		note: string;
	};
	conclusion: string;
}

export interface CollectorArtifacts {
	evidence: SwitchableBaselineEvidence;
	jsonEvidence: string;
	jsonSha256: string;
	markdownEvidence: string;
}

export function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function compareText(left: string, right: string): number {
	if (left < right) return -1;
	if (left > right) return 1;
	return 0;
}
