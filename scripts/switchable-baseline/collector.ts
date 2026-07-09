import {
	materializePrometheusSignals,
	parseBaselineCatalog,
} from "./catalog";
import {
	buildSwitchableBaselineEvidence,
	renderEvidenceMarkdown,
	serializeEvidence,
	sha256Hex,
} from "./evidence";
import {
	CATALOG_PATH,
	type CollectionRequest,
	type CollectorArtifacts,
	REPOSITORY_ROOT,
} from "./model";
import {
	collectPrometheusSignals,
	type FetchLike,
} from "./prometheus";
import {
	parseCollectorArguments,
	readPrometheusConfiguration,
	validateCollectionRequest,
} from "./request";
import { parseJsonDocument } from "./json";

export interface CollectBaselineInput {
	rawCatalog: string;
	argumentsList: string[];
	environment: Record<string, string | undefined>;
	fetcher?: FetchLike;
	catalogAtCommit?: CatalogAtCommitReader;
}

export type CatalogAtCommitReader = (commit: string) => Promise<string>;

export async function readCatalogAtCommit(commit: string): Promise<string> {
	const repositoryPath = CATALOG_PATH.slice(REPOSITORY_ROOT.length + 1);
	const process = Bun.spawn(["git", "show", `${commit}:${repositoryPath}`], {
		cwd: REPOSITORY_ROOT,
		stdout: "pipe",
		stderr: "ignore",
	});
	const [exitCode, stdout] = await Promise.all([
		process.exited,
		new Response(process.stdout).text(),
	]);
	if (exitCode !== 0) {
		throw new Error("could not read the baseline catalog at the asserted commit");
	}
	return stdout;
}

export interface CollectorResult extends CollectorArtifacts {
	request: CollectionRequest;
}

export async function collectBaselineArtifacts({
	rawCatalog,
	argumentsList,
	environment,
	fetcher,
	catalogAtCommit = readCatalogAtCommit,
}: CollectBaselineInput): Promise<CollectorResult> {
	const catalogValue = parseJsonDocument(rawCatalog, "baseline signal catalog");
	const catalog = parseBaselineCatalog(catalogValue);
	const argumentsValue = parseCollectorArguments(argumentsList);
	const request = validateCollectionRequest(
		argumentsValue,
		catalog.minimumCollectionWindowMinutes,
		catalog.deploymentScope.maximumRangeLookbackSeconds,
	);
	const committedCatalog = await catalogAtCommit(request.serverCommit);
	if (committedCatalog !== rawCatalog) {
		throw new Error(
			"baseline catalog bytes do not match the catalog at the asserted commit",
		);
	}
	const configuration = readPrometheusConfiguration(environment);
	const signals = materializePrometheusSignals(catalog, request);
	const collectedSignals = await collectPrometheusSignals(
		catalog,
		signals,
		request,
		configuration,
		fetcher,
	);
	const evidence = buildSwitchableBaselineEvidence(
		catalog,
		sha256Hex(rawCatalog),
		request,
		collectedSignals,
	);
	const jsonEvidence = serializeEvidence(evidence);
	const jsonSha256 = sha256Hex(jsonEvidence);
	const markdownEvidence = renderEvidenceMarkdown(evidence, jsonSha256);

	return {
		request,
		evidence,
		jsonEvidence,
		jsonSha256,
		markdownEvidence,
	};
}
