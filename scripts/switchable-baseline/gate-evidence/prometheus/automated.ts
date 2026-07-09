import {
  fail,
  requireExactKeys,
  requireRecord,
  requireString,
  type EvidenceDeploymentScope,
  type EvidenceWindow,
} from "../common";
import type { CatalogSignal, TrustedCatalog } from "../catalog";
import {
  validatePrometheusSeries,
  validateSignalSafety,
} from "./series";
import { compareText } from "../../model";

function materializeCatalogSignal(
  signal: CatalogSignal,
  scope: EvidenceDeploymentScope,
  commit: string,
): CatalogSignal {
  const selector = '{job="' + scope.job
    + '",deployment_environment="' + scope.environment
    + '",cluster="' + scope.cluster
    + '",namespace="' + scope.namespace + '"}';
  const replacements: Record<string, string> = {
    "{{commit}}": commit,
    "{{cluster}}": scope.cluster,
    "{{environment}}": scope.environment,
  };
  return {
    ...signal,
    query: signal.query.replaceAll("{{scope}}", selector),
    attributes: Object.fromEntries(
      Object.entries(signal.attributes).map(([key, values]) => [
        key,
        values.map((value) => replacements[value] ?? value),
      ]),
    ),
  };
}

export function validateAutomatedPrometheus(
  value: unknown,
  catalog: TrustedCatalog,
  scope: EvidenceDeploymentScope,
  serverCommit: string,
  parsedWindow: EvidenceWindow,
  stepSeconds: number,
  label: string,
): void {
  const automated = requireRecord(
    value,
    label + ".automatedPrometheus",
  );
  requireExactKeys(
    automated,
    ["status", "signals"],
    label + ".automatedPrometheus",
  );
  if (automated.status !== "collected") {
    fail(label + ".automatedPrometheus.status must be collected");
  }
  if (!Array.isArray(automated.signals)) {
    fail(label + ".automatedPrometheus.signals must be an array");
  }
  const expectedSignals = catalog.signals
    .filter(
      ({ source, collection }) =>
        source === "prometheus" && collection === "automated",
    )
    .map((signal) =>
      materializeCatalogSignal(signal, scope, serverCommit)
    )
    .sort((left, right) => compareText(left.id, right.id));
  const actualIds = automated.signals.map((entry, index) =>
    requireString(
      requireRecord(
        entry,
        label + ".automatedPrometheus.signals[" + index + "]",
      ),
      "id",
      label,
    )
  );
  if (
    JSON.stringify(actualIds)
    !== JSON.stringify(expectedSignals.map(({ id }) => id))
  ) {
    fail(
      label
        + ".automatedPrometheus.signals must be the complete sorted catalog set",
    );
  }
  for (const [index, signal] of expectedSignals.entries()) {
    const signalLabel =
      label + ".automatedPrometheus.signals[" + index + "]";
    const observed = requireRecord(automated.signals[index], signalLabel);
    const signalKeys = [
      "id",
      "query",
      "window",
      "unit",
      "interpretation",
      "limitations",
      "series",
    ];
    if (signal.minimumAllowedValue !== undefined) {
      signalKeys.push("minimumAllowedValue");
    }
    if (signal.maximumAllowedValue !== undefined) {
      signalKeys.push("maximumAllowedValue");
    }
    if (signal.collectionLookbackSeconds !== undefined) {
      signalKeys.push("collectionLookbackSeconds");
    }
    if (signal.requiredStability !== undefined) {
      signalKeys.push("requiredStability");
    }
    requireExactKeys(observed, signalKeys, signalLabel);
    for (
      const key of [
        "query",
        "window",
        "unit",
        "interpretation",
        "limitations",
      ] as const
    ) {
      if (observed[key] !== signal[key]) {
        fail(signalLabel + "." + key + " must match the catalog");
      }
    }
    if (
      observed.minimumAllowedValue !== signal.minimumAllowedValue
      || observed.maximumAllowedValue !== signal.maximumAllowedValue
    ) {
      fail(signalLabel + " allowed-value bounds must match the catalog");
    }
    if (
      observed.collectionLookbackSeconds !== signal.collectionLookbackSeconds
      || observed.requiredStability !== signal.requiredStability
    ) {
      fail(signalLabel + " collection continuity fields must match the catalog");
    }
    const series = validatePrometheusSeries(
      observed.series,
      signal,
      parsedWindow,
      stepSeconds,
      signal.collectionLookbackSeconds ?? 0,
      signalLabel + ".series",
    );
    validateSignalSafety(signal, series, scope, signalLabel);
	}
}
