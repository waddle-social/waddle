import {
  type BaselineCatalog,
  type BaselineSignal,
  compareText,
  type CollectionRequest,
  DEPLOYMENT_SCOPE_BINDINGS,
  GATE_ZERO_FARO_SIGNAL_IDS,
  GATE_ZERO_PROMETHEUS_SIGNAL_IDS,
  IDENTITY_ATTRIBUTE_BINDINGS,
  SCOPE_QUERY_PLACEHOLDER,
} from "../model";

export function assertGateZeroSignalSet(catalog: BaselineCatalog): void {
	const prometheusIds = catalog.signals
		.filter(({ source, collection }) => source === "prometheus" && collection === "automated")
		.map(({ id }) => id)
		.sort(compareText);
	const faroIds = catalog.signals
		.filter(({ source, collection }) => source === "faro" && collection === "manual-export")
		.map(({ id }) => id)
		.sort(compareText);
	if (JSON.stringify(prometheusIds) !== JSON.stringify([...GATE_ZERO_PROMETHEUS_SIGNAL_IDS])) {
		throw new Error("catalog must define the exact Gate 0 Prometheus signal set");
	}
	if (JSON.stringify(faroIds) !== JSON.stringify([...GATE_ZERO_FARO_SIGNAL_IDS])) {
		throw new Error("catalog must define the exact Gate 0 Faro signal set");
	}
}

export function selectAutomatedPrometheusSignals(
	catalog: BaselineCatalog,
): BaselineSignal[] {
	return catalog.signals
		.filter(
			({ source, collection }) =>
				source === "prometheus" && collection === "automated",
		)
		.sort((left, right) => compareText(left.id, right.id));
}

function escapePrometheusString(value: string): string {
	return value
		.replaceAll("\\", "\\\\")
		.replaceAll("\n", "\\n")
		.replaceAll('"', '\\"');
}

export function materializePrometheusSignal(
	signal: BaselineSignal,
	request: CollectionRequest,
): BaselineSignal {
	const scope = `{${DEPLOYMENT_SCOPE_BINDINGS.map(
		({ label, requestKey }) =>
			`${label}="${escapePrometheusString(String(request[requestKey]))}"`,
	).join(",")}}`;
	const query = signal.query.replaceAll(SCOPE_QUERY_PLACEHOLDER, scope);
	if (/{{[^{}]+}}/.test(query)) {
		throw new Error(
			`automated Prometheus signal ${signal.id} retains an unresolved placeholder`,
		);
	}
	const attributes = Object.fromEntries(
		Object.entries(signal.attributes).map(([key, values]) => [
			key,
			values.map((value) => {
				const binding = IDENTITY_ATTRIBUTE_BINDINGS.find(
					({ placeholder }) => value === placeholder,
				);
				return binding === undefined
					? value
					: String(request[binding.requestKey]);
			}),
		]),
	);
	return { ...signal, attributes, query };
}

export function materializePrometheusSignals(
	catalog: BaselineCatalog,
	request: CollectionRequest,
): BaselineSignal[] {
	return selectAutomatedPrometheusSignals(catalog).map((signal) =>
		materializePrometheusSignal(signal, request),
	);
}
