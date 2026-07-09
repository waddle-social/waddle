import {
	type BaselineCatalog,
	type BaselineSignal,
	compareText,
	type CollectedPrometheusSignal,
	type CollectionRequest,
	type EvidenceSample,
	type EvidenceSeries,
	isRecord,
	type PrometheusConfiguration,
	type PrometheusQueryRangePayload,
	QUERY_STEP_SECONDS,
} from "./model";

export type FetchLike = (
	input: string | URL | Request,
	init?: RequestInit,
) => Promise<Response>;

export function buildQueryRangeUrl(
	baseUrl: string,
	signal: BaselineSignal,
	request: CollectionRequest,
): URL {
	const url = new URL(baseUrl);
	const queryRangeSuffix = "/api/v1/query_range";
	const path = url.pathname.replace(/\/+$/, "");
	url.pathname = path.endsWith(queryRangeSuffix)
		? path
		: `${path}${queryRangeSuffix}`;
	url.searchParams.set("query", signal.query);
	url.searchParams.set("start", String(request.startEpochSeconds));
	url.searchParams.set("end", String(request.endEpochSeconds));
	url.searchParams.set("step", String(QUERY_STEP_SECONDS));
	return url;
}

function normalizeFiniteNumber(value: unknown): number | undefined {
	if (typeof value === "number") {
		return Number.isFinite(value) ? value : undefined;
	}
	if (
		typeof value !== "string" ||
		!/^[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[-+]?\d+)?$/i.test(value)
	) {
		return undefined;
	}
	const number = Number(value);
	return Number.isFinite(number) ? number : undefined;
}

export function expectedTimestampGrid(
	request: CollectionRequest,
): number[] {
	const timestamps: number[] = [];
	for (
		let timestamp = request.startEpochSeconds;
		timestamp <= request.endEpochSeconds;
		timestamp += QUERY_STEP_SECONDS
	) {
		timestamps.push(timestamp);
	}
	return timestamps;
}

export function signalCollectionRequest(
	signal: BaselineSignal,
	request: CollectionRequest,
): CollectionRequest {
	return {
		...request,
		startEpochSeconds:
			request.startEpochSeconds - (signal.collectionLookbackSeconds ?? 0),
	};
}

function normalizeSamples(
	signalId: string,
	value: unknown,
	request: CollectionRequest,
): { samples: EvidenceSample[]; canonicalEndSample: EvidenceSample } {
	if (!Array.isArray(value) || value.length === 0) {
		throw new Error(`Prometheus returned no samples for signal ${signalId}`);
	}

	const samples = value.map((sample) => {
		if (!Array.isArray(sample) || sample.length !== 2) {
			throw new Error(
				`Prometheus returned an invalid sample for signal ${signalId}`,
			);
		}
		const timestamp = normalizeFiniteNumber(sample[0]);
		const sampleValue = normalizeFiniteNumber(sample[1]);
		if (timestamp === undefined || sampleValue === undefined) {
			throw new Error(
				`Prometheus returned a non-finite sample for signal ${signalId}`,
			);
		}
		return { timestamp, value: sampleValue };
	});

	samples.sort((left, right) => left.timestamp - right.timestamp);
	const expectedTimestamps = expectedTimestampGrid(request);
	if (
		samples.length !== expectedTimestamps.length ||
		samples.some(
			(sample, index) => sample.timestamp !== expectedTimestamps[index],
		)
	) {
		throw new Error(
			`Prometheus returned an incomplete timestamp grid for signal ${signalId}`,
		);
	}

	return {
		samples,
		canonicalEndSample: samples.at(-1) as EvidenceSample,
	};
}

function attributeSortKey(attributes: Record<string, string>): string {
	return JSON.stringify(Object.entries(attributes));
}

function normalizeAttributes(
	signal: BaselineSignal,
	value: unknown,
): Record<string, string> {
	if (!isRecord(value)) {
		throw new Error(
			`Prometheus returned invalid attributes for signal ${signal.id}`,
		);
	}

	const keys = Object.keys(value).sort(compareText);
	const expectedKeys = Object.keys(signal.attributes).sort(compareText);
	if (JSON.stringify(keys) !== JSON.stringify(expectedKeys)) {
		throw new Error(
			`Prometheus returned attributes that do not exactly match the catalog for signal ${signal.id}`,
		);
	}

	const attributes: Record<string, string> = {};
	for (const key of keys) {
		const attributeValue = value[key];
		if (
			typeof attributeValue !== "string" ||
			!signal.attributes[key].includes(attributeValue)
		) {
			throw new Error(
				`Prometheus returned an undeclared attribute value for signal ${signal.id}`,
			);
		}
		attributes[key] = attributeValue;
	}
	return attributes;
}

export function requiredAttributeCombinations(
	signal: BaselineSignal,
): Record<string, string>[] {
	const entries = Object.entries(signal.attributes).sort(([left], [right]) =>
		compareText(left, right),
	);
	let combinations: Record<string, string>[] = [{}];
	for (const [key, values] of entries) {
		combinations = combinations.flatMap((combination) =>
			values.map((value) => ({ ...combination, [key]: value })),
		);
	}
	return combinations.sort((left, right) =>
		compareText(attributeSortKey(left), attributeSortKey(right)),
	);
}

function assertRequiredSeries(
	signal: BaselineSignal,
	series: EvidenceSeries[],
): void {
	const actualKeys = series.map(({ attributes }) => attributeSortKey(attributes));
	const expectedKeys = requiredAttributeCombinations(signal).map(attributeSortKey);
	if (JSON.stringify(actualKeys) !== JSON.stringify(expectedKeys)) {
		throw new Error(
			`Prometheus did not return the exact required series combinations for signal ${signal.id}`,
		);
	}
}

export function normalizeQueryRangeResponse(
	signal: BaselineSignal,
	payload: PrometheusQueryRangePayload,
	request: CollectionRequest,
): CollectedPrometheusSignal {
	for (const field of ["warnings", "infos"] as const) {
		const messages = payload[field];
		if (messages === undefined) continue;
		if (!Array.isArray(messages)) {
			throw new Error(`Prometheus returned an invalid ${field} field for signal ${signal.id}`);
		}
		if (messages.length > 0) {
			throw new Error(
				`Prometheus returned ${field} for signal ${signal.id}; partial or qualified query results are not valid evidence`,
			);
		}
	}
	if (payload.status !== "success" || !isRecord(payload.data)) {
		throw new Error(`Prometheus query did not succeed for signal ${signal.id}`);
	}
	if (
		payload.data.resultType !== "matrix" ||
		!Array.isArray(payload.data.result)
	) {
		throw new Error(
			`Prometheus returned a non-matrix result for signal ${signal.id}`,
		);
	}
	if (payload.data.result.length === 0) {
		throw new Error(`Prometheus returned no series for signal ${signal.id}`);
	}

	const series = payload.data.result.map((item) => {
		if (!isRecord(item)) {
			throw new Error(
				`Prometheus returned an invalid series for signal ${signal.id}`,
			);
		}
		const attributes = normalizeAttributes(signal, item.metric);
		const { samples, canonicalEndSample } = normalizeSamples(
			signal.id,
			item.values,
			request,
		);
		return { attributes, samples, canonicalEndSample };
	});

	series.sort((left, right) =>
		compareText(
			attributeSortKey(left.attributes),
			attributeSortKey(right.attributes),
		),
	);
	assertRequiredSeries(signal, series);
	for (const { samples } of series) {
		if (
			signal.requiredStability === "constant"
			&& samples.some(({ value }) => value !== samples[0].value)
		) {
			throw new Error(
				`Prometheus signal ${signal.id} must remain constant across the complete collection grid`,
			);
		}
		for (const sample of samples) {
			if (
				signal.minimumAllowedValue !== undefined &&
				sample.value < signal.minimumAllowedValue
			) {
				throw new Error(
					`Prometheus value ${sample.value} for signal ${signal.id} is below required minimum ${signal.minimumAllowedValue}`,
				);
			}
			if (
				signal.maximumAllowedValue !== undefined &&
				sample.value > signal.maximumAllowedValue
			) {
				throw new Error(
					`Prometheus value ${sample.value} for signal ${signal.id} exceeds required maximum ${signal.maximumAllowedValue}`,
				);
			}
		}
	}

	return {
		id: signal.id,
		query: signal.query,
		window: signal.window,
		unit: signal.unit,
		...(signal.collectionLookbackSeconds === undefined
			? {}
			: { collectionLookbackSeconds: signal.collectionLookbackSeconds }),
		...(signal.requiredStability === undefined
			? {}
			: { requiredStability: signal.requiredStability }),
		...(signal.minimumAllowedValue === undefined
			? {}
			: { minimumAllowedValue: signal.minimumAllowedValue }),
		...(signal.maximumAllowedValue === undefined
			? {}
			: { maximumAllowedValue: signal.maximumAllowedValue }),
		interpretation: signal.interpretation,
		limitations: signal.limitations,
		series,
	};
}

export function assertDeploymentIdentity(
	catalog: BaselineCatalog,
	request: CollectionRequest,
	collectedSignals: CollectedPrometheusSignal[],
): void {
	const targets = collectedSignals.filter(
		({ id }) => id === catalog.deploymentScope.targetSignalId,
	);
	if (targets.length !== 1 || targets[0].series.length !== 1) {
		throw new Error("Prometheus deployment identity target is not singular");
	}
	for (const sample of targets[0].series[0].samples) {
		if (sample.value !== request.expectedReplicas) {
			throw new Error(
				`Prometheus deployment identity target count ${sample.value} does not match expected replicas ${request.expectedReplicas}`,
			);
		}
	}
}

async function collectSignal(
	signal: BaselineSignal,
	request: CollectionRequest,
	configuration: PrometheusConfiguration,
	authorization: string,
	fetcher: FetchLike,
): Promise<CollectedPrometheusSignal> {
	const signalRequest = signalCollectionRequest(signal, request);
	const url = buildQueryRangeUrl(configuration.baseUrl, signal, signalRequest);
	let response: Response;
	try {
		response = await fetcher(url, {
			headers: {
				Accept: "application/json",
				Authorization: `Basic ${authorization}`,
			},
			signal: AbortSignal.timeout(60_000),
		});
	} catch {
		throw new Error(`Prometheus request failed for signal ${signal.id}`);
	}
	if (!response.ok) {
		throw new Error(
			`Prometheus request failed for signal ${signal.id} with HTTP ${response.status}`,
		);
	}

	let payload: PrometheusQueryRangePayload;
	try {
		payload = (await response.json()) as PrometheusQueryRangePayload;
	} catch {
		throw new Error(`Prometheus returned invalid JSON for signal ${signal.id}`);
	}
	return normalizeQueryRangeResponse(signal, payload, signalRequest);
}

export async function collectPrometheusSignals(
	catalog: BaselineCatalog,
	signals: BaselineSignal[],
	request: CollectionRequest,
	configuration: PrometheusConfiguration,
	fetcher: FetchLike = fetch,
): Promise<CollectedPrometheusSignal[]> {
	const authorization = Buffer.from(
		`${configuration.username}:${configuration.apiKey}`,
		"utf8",
	).toString("base64");
	const collected = await Promise.all(
		signals.map((signal) =>
			collectSignal(
				signal,
				request,
				configuration,
				authorization,
				fetcher,
			),
		),
	);
	collected.sort((left, right) => compareText(left.id, right.id));
	assertDeploymentIdentity(catalog, request, collected);
	return collected;
}
