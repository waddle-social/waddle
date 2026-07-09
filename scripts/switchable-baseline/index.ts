export {
	materializePrometheusSignal,
	materializePrometheusSignals,
	assertGateZeroSignalSet,
	parseBaselineCatalog,
	selectAutomatedPrometheusSignals,
} from "./catalog";
export {
	collectBaselineArtifacts,
	type CollectBaselineInput,
	type CatalogAtCommitReader,
	type CollectorResult,
	readCatalogAtCommit,
} from "./collector";
export {
	buildSwitchableBaselineEvidence,
	renderEvidenceMarkdown,
	serializeEvidence,
	sha256Hex,
} from "./evidence";
export type {
	BaselineCatalog,
	BaselineSignal,
	CollectedPrometheusSignal,
	CollectionRequest,
	CollectorArguments,
	CollectorArtifacts,
	EvidenceSample,
	EvidenceSeries,
	PrivacyContract,
	PrometheusConfiguration,
	SwitchableBaselineEvidence,
} from "./model";
export {
	CATALOG_PATH,
	DEFAULT_OUTPUT_DIRECTORY,
	IDENTITY_ATTRIBUTE_BINDINGS,
	IDENTITY_COMMIT_PLACEHOLDER,
	JSON_EVIDENCE_FILENAME,
	MARKDOWN_EVIDENCE_FILENAME,
	QUERY_STEP_SECONDS,
	REPOSITORY_ROOT,
	SCOPE_QUERY_PLACEHOLDER,
	SWITCHABLE_BASELINE_INPUT_DIRECTORY,
} from "./model";
export {
	assertDeploymentIdentity,
	buildQueryRangeUrl,
	collectPrometheusSignals,
	expectedTimestampGrid,
	normalizeQueryRangeResponse,
	requiredAttributeCombinations,
	signalCollectionRequest,
	type FetchLike,
} from "./prometheus";
export {
	parseCollectorArguments,
	readPrometheusConfiguration,
	validateCollectionRequest,
} from "./request";
export {
	type EvidencePaths,
	writeEvidencePairAtomically,
} from "./writer";
export {
	verifyGateZeroEvidencePackage,
} from "./finalize";
export { finalizeGateZeroEvidence } from "./finalize/all";
export {
	parseReleaseArtifactProvenance,
	serverReleaseArtifactSetSha256,
	verifyTrustedReleaseArtifactProvenance,
	webReleaseArtifactSetSha256,
	type ReleaseArtifactProvenance,
	type ReleaseArtifactProvenanceVerifier,
} from "./release-artifact-provenance";
export {
	buildJourneyBaselineManifest,
	validateJourneyBaselineManifestReference,
} from "./journey-baseline";
