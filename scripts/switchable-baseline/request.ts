import { resolve } from "node:path";
import {
	type CollectionRequest,
	type CollectorArguments,
	DEFAULT_OUTPUT_DIRECTORY,
	type PrometheusConfiguration,
	QUERY_STEP_SECONDS,
} from "./model";

function readOption(
	argumentsList: string[],
	index: number,
): { key: string; value: string; consumed: number } {
	const argument = argumentsList[index];
	if (!argument.startsWith("--")) {
		throw new Error(`unexpected positional argument at index ${index + 1}`);
	}

	const equalsIndex = argument.indexOf("=");
	if (equalsIndex >= 0) {
		const key = argument.slice(2, equalsIndex);
		const value = argument.slice(equalsIndex + 1);
		if (value.length === 0) throw new Error(`--${key} requires a value`);
		return { key, value, consumed: 1 };
	}

	const key = argument.slice(2);
	const value = argumentsList[index + 1];
	if (value === undefined || value.startsWith("--")) {
		throw new Error(`--${key} requires a value`);
	}
	return { key, value, consumed: 2 };
}

export function parseCollectorArguments(
	argumentsList: string[],
): CollectorArguments {
	const supported = new Set([
		"start",
		"end",
		"server-commit",
		"prometheus-job",
		"environment",
		"cluster",
		"namespace",
		"expected-replicas",
		"output-dir",
	]);
	const options = new Map<string, string>();
	for (let index = 0; index < argumentsList.length; ) {
		const option = readOption(argumentsList, index);
		if (!supported.has(option.key)) {
			throw new Error(`unknown option --${option.key}`);
		}
		if (options.has(option.key)) {
			throw new Error(`option --${option.key} was provided more than once`);
		}
		options.set(option.key, option.value);
		index += option.consumed;
	}

	for (const required of [
		"start",
		"end",
		"server-commit",
		"prometheus-job",
		"environment",
		"cluster",
		"namespace",
		"expected-replicas",
	]) {
		if (!options.has(required)) {
			throw new Error(`missing required option --${required}`);
		}
	}

	return {
		start: options.get("start") as string,
		end: options.get("end") as string,
		serverCommit: options.get("server-commit") as string,
		job: options.get("prometheus-job") as string,
		environment: options.get("environment") as string,
		cluster: options.get("cluster") as string,
		namespace: options.get("namespace") as string,
		expectedReplicas: options.get("expected-replicas") as string,
		outputDirectory: resolve(
			options.get("output-dir") ?? DEFAULT_OUTPUT_DIRECTORY,
		),
	};
}

function normalizeIsoInstant(value: string, optionName: string): string {
	const match = value.match(
		/^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/,
	);
	if (!match) {
		throw new Error(
			`--${optionName} must be an ISO 8601 instant with an explicit timezone`,
		);
	}
	const [, yearText, monthText, dayText, hourText, minuteText, secondText] =
		match;
	const year = Number(yearText);
	const month = Number(monthText);
	const day = Number(dayText);
	const hour = Number(hourText);
	const minute = Number(minuteText);
	const second = Number(secondText);
	const leapYear = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
	const daysByMonth = [
		31,
		leapYear ? 29 : 28,
		31,
		30,
		31,
		30,
		31,
		31,
		30,
		31,
		30,
		31,
	];
	if (
		month < 1 ||
		month > 12 ||
		day < 1 ||
		day > daysByMonth[month - 1] ||
		hour > 23 ||
		minute > 59 ||
		second > 59
	) {
		throw new Error(`--${optionName} is not a valid ISO 8601 instant`);
	}
	const epochMilliseconds = Date.parse(value);
	if (!Number.isFinite(epochMilliseconds)) {
		throw new Error(`--${optionName} is not a valid ISO 8601 instant`);
	}
	return new Date(epochMilliseconds).toISOString();
}

function validateScopeValue(value: string, optionName: string): string {
	if (value === "unknown") {
		throw new Error(`--${optionName} must not be unknown`);
	}
	if (!/^[a-z0-9](?:[a-z0-9._-]{0,62}[a-z0-9])?$/.test(value)) {
		throw new Error(
			`--${optionName} must be a lowercase deployment label value`,
		);
	}
	return value;
}

function parseExpectedReplicas(value: string): number {
	if (!/^[1-9][0-9]*$/.test(value)) {
		throw new Error("--expected-replicas must be a positive integer");
	}
	const replicas = Number(value);
	if (!Number.isSafeInteger(replicas) || replicas > 10_000) {
		throw new Error("--expected-replicas must be between 1 and 10000");
	}
	return replicas;
}

export function validateCollectionRequest(
	argumentsValue: CollectorArguments,
	minimumCollectionWindowMinutes: number,
	maximumRangeLookbackSeconds: number,
): CollectionRequest {
	if (resolve(argumentsValue.outputDirectory) !== DEFAULT_OUTPUT_DIRECTORY) {
		throw new Error(
			`--output-dir must be the canonical staging directory ${DEFAULT_OUTPUT_DIRECTORY}`,
		);
	}
	if (
		!Number.isInteger(minimumCollectionWindowMinutes) ||
		minimumCollectionWindowMinutes <= 0
	) {
		throw new Error("minimum collection window must be a positive integer");
	}
	if (
		!Number.isInteger(maximumRangeLookbackSeconds) ||
		maximumRangeLookbackSeconds <= 0 ||
		maximumRangeLookbackSeconds % QUERY_STEP_SECONDS !== 0
	) {
		throw new Error(
			"maximum range lookback must be a positive whole number of query steps",
		);
	}
	if (!/^[0-9a-f]{40}$/.test(argumentsValue.serverCommit)) {
		throw new Error(
			"--server-commit must be a full 40-character lowercase Git commit SHA",
		);
	}

	const start = normalizeIsoInstant(argumentsValue.start, "start");
	const end = normalizeIsoInstant(argumentsValue.end, "end");
	const startEpochSeconds = Date.parse(start) / 1_000;
	const endEpochSeconds = Date.parse(end) / 1_000;
	const durationSeconds = endEpochSeconds - startEpochSeconds;
	const durationMinutes = durationSeconds / 60;
	if (durationMinutes < minimumCollectionWindowMinutes) {
		throw new Error(
			`collection window must be at least ${minimumCollectionWindowMinutes} minutes`,
		);
	}
	if (!Number.isInteger(durationMinutes)) {
		throw new Error("collection window must span a whole number of minutes");
	}
	if (
		durationSeconds % QUERY_STEP_SECONDS !== 0
		|| startEpochSeconds % QUERY_STEP_SECONDS !== 0
		|| endEpochSeconds % QUERY_STEP_SECONDS !== 0
	) {
		throw new Error("collection window must align to the Prometheus query step");
	}

	return {
		start,
		end,
		startEpochSeconds,
		endEpochSeconds,
		durationMinutes,
		serverCommit: argumentsValue.serverCommit,
		job: validateScopeValue(argumentsValue.job, "prometheus-job"),
		environment: validateScopeValue(argumentsValue.environment, "environment"),
		cluster: validateScopeValue(argumentsValue.cluster, "cluster"),
		namespace: validateScopeValue(argumentsValue.namespace, "namespace"),
		identityStartEpochSeconds:
			startEpochSeconds - maximumRangeLookbackSeconds,
		expectedReplicas: parseExpectedReplicas(argumentsValue.expectedReplicas),
		outputDirectory: argumentsValue.outputDirectory,
	};
}

export function readPrometheusConfiguration(
	environment: Record<string, string | undefined>,
): PrometheusConfiguration {
	const required = [
		"GRAFANA_PROMETHEUS_URL",
		"GRAFANA_PROMETHEUS_USER",
		"GRAFANA_PROMETHEUS_API_KEY",
	] as const;
	const missing = required.filter((name) => !environment[name]);
	if (missing.length > 0) {
		throw new Error(
			`missing required environment variables: ${missing.join(", ")}`,
		);
	}

	const baseUrl = environment.GRAFANA_PROMETHEUS_URL as string;
	const username = environment.GRAFANA_PROMETHEUS_USER as string;
	const apiKey = environment.GRAFANA_PROMETHEUS_API_KEY as string;
	let parsedUrl: URL;
	try {
		parsedUrl = new URL(baseUrl);
	} catch {
		throw new Error("GRAFANA_PROMETHEUS_URL must be a valid HTTPS URL");
	}
	if (parsedUrl.protocol !== "https:") {
		throw new Error("GRAFANA_PROMETHEUS_URL must use HTTPS");
	}
	if (parsedUrl.username || parsedUrl.password) {
		throw new Error("GRAFANA_PROMETHEUS_URL must not contain credentials");
	}
	if (parsedUrl.search || parsedUrl.hash) {
		throw new Error(
			"GRAFANA_PROMETHEUS_URL must not contain a query or fragment",
		);
	}
	if (/[:\r\n]/.test(username) || /[\r\n]/.test(apiKey)) {
		throw new Error("Prometheus credentials contain unsupported characters");
	}

	return { baseUrl, username, apiKey };
}
