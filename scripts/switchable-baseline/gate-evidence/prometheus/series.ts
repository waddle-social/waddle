import {
  fail,
  requireExactKeys,
  requireFiniteNumber,
  requireInteger,
  requireRecord,
  requireString,
  type EvidenceDeploymentScope,
  type EvidenceWindow,
} from "../common";
import type { CatalogSignal } from "../catalog";

interface EvidenceSample {
  timestamp: number;
  value: number;
}

interface ValidatedSeries {
  attributes: Record<string, string>;
  samples: EvidenceSample[];
}

export interface ValidatedPrometheusArtifact {
  scope: EvidenceDeploymentScope;
  catalog: TrustedCatalog;
}

function cartesianAttributes(
  attributes: Record<string, string[]>,
): Record<string, string>[] {
  let combinations: Record<string, string>[] = [{}];
  for (const key of Object.keys(attributes).sort()) {
    combinations = combinations.flatMap((existing) =>
      [...attributes[key]].sort().map((value) => ({
        ...existing,
        [key]: value,
      }))
    );
  }
  return combinations;
}

function canonicalAttributes(attributes: Record<string, string>): string {
  return JSON.stringify(
    Object.fromEntries(Object.entries(attributes).sort()),
  );
}

function parseEvidenceSample(
  value: unknown,
  label: string,
): EvidenceSample {
  const sample = requireRecord(value, label);
  requireExactKeys(sample, ["timestamp", "value"], label);
  return {
    timestamp: requireInteger(sample.timestamp, label + ".timestamp", 1),
    value: requireFiniteNumber(sample.value, label + ".value"),
  };
}

export function validatePrometheusSeries(
  value: unknown,
  signal: CatalogSignal,
  window: EvidenceWindow,
  stepSeconds: number,
  startLookbackSeconds: number,
  label: string,
): ValidatedSeries[] {
  if (!Array.isArray(value)) fail(label + " must be an array");
  const start = Date.parse(window.start) / 1_000 - startLookbackSeconds;
  const end = Date.parse(window.end) / 1_000;
  const expectedTimestamps: number[] = [];
  for (
    let timestamp = start;
    timestamp <= end;
    timestamp += stepSeconds
  ) {
    expectedTimestamps.push(timestamp);
  }
  const parsed = value.map((entry, index): ValidatedSeries => {
    const seriesLabel = label + "[" + index + "]";
    const series = requireRecord(entry, seriesLabel);
    requireExactKeys(
      series,
      ["attributes", "samples", "canonicalEndSample"],
      seriesLabel,
    );
    const attributes = requireRecord(
      series.attributes,
      seriesLabel + ".attributes",
    );
    requireExactKeys(
      attributes,
      Object.keys(signal.attributes),
      seriesLabel + ".attributes",
    );
    const parsedAttributes: Record<string, string> = {};
    for (const [key, allowed] of Object.entries(signal.attributes)) {
      const attribute = requireString(
        attributes,
        key,
        seriesLabel + ".attributes",
      );
      if (!allowed.includes(attribute)) {
        fail(
          seriesLabel + ".attributes." + key
            + " is outside the catalog closed set",
        );
      }
      parsedAttributes[key] = attribute;
    }
    if (!Array.isArray(series.samples)) {
      fail(seriesLabel + ".samples must be an array");
    }
    const samples = series.samples.map((sample, sampleIndex) =>
      parseEvidenceSample(
        sample,
        seriesLabel + ".samples[" + sampleIndex + "]",
      )
    );
    if (
      JSON.stringify(samples.map(({ timestamp }) => timestamp))
      !== JSON.stringify(expectedTimestamps)
    ) {
      fail(
        seriesLabel
          + ".samples must cover the complete fixed timestamp grid",
      );
    }
    const canonicalEndSample = parseEvidenceSample(
      series.canonicalEndSample,
      seriesLabel + ".canonicalEndSample",
    );
    const lastSample = samples.at(-1);
    if (
      !lastSample
      || JSON.stringify(canonicalEndSample) !== JSON.stringify(lastSample)
    ) {
      fail(
        seriesLabel
          + ".canonicalEndSample must equal the final range sample",
      );
    }
    return { attributes: parsedAttributes, samples };
  });
  const actualCombinations = parsed
    .map(({ attributes }) => canonicalAttributes(attributes))
    .sort();
  const expectedCombinations = cartesianAttributes(signal.attributes)
    .map(canonicalAttributes)
    .sort();
  if (
    JSON.stringify(actualCombinations)
    !== JSON.stringify(expectedCombinations)
  ) {
    fail(
      label
        + " must contain every catalogued attribute combination exactly once",
    );
  }
  return parsed;
}

export function validateSignalSafety(
  signal: CatalogSignal,
  series: ValidatedSeries[],
  scope: EvidenceDeploymentScope,
  label: string,
): void {
  for (const entry of series) {
    if (
      signal.requiredStability === "constant"
      && entry.samples.some(({ value }) => value !== entry.samples[0].value)
    ) {
      fail(label + " must remain constant across the complete collection grid");
    }
    for (const sample of entry.samples) {
      if (
        signal.minimumAllowedValue !== undefined
        && sample.value < signal.minimumAllowedValue
      ) {
        fail(label + " contains a value below the catalog minimum");
      }
      if (
        signal.maximumAllowedValue !== undefined
        && sample.value > signal.maximumAllowedValue
      ) {
        fail(label + " contains a value above the catalog maximum");
      }
    }
  }
  if (
    signal.id === scope.targetSignalId
    && series.some((entry) =>
      entry.samples.some(({ value }) => value !== scope.expectedReplicas)
    )
  ) {
    fail(label + " must report the exact expected replica count at every sample");
  }
  if (
    signal.id === "loss-corruption-safety"
    && series.some((entry) =>
      entry.samples.some(({ value }) => value !== 0)
    )
  ) {
    fail(
      label
        + " must be zero on every permanent-loss and corruption surface",
    );
  }
  if (signal.id === "live-delivery-channel-outcomes") {
    for (const entry of series) {
      if (
        entry.attributes.outcome !== "delivered"
        && entry.samples.some(({ value }) => value !== 0)
      ) {
        fail(
          label
            + " must record zero dropped_full and dropped_closed outcomes",
        );
      }
    }
  }
  for (const criterion of signal.requiredActivity ?? []) {
    const expected = canonicalAttributes(criterion.attributes);
    const entry = series.find(({ attributes }) => canonicalAttributes(attributes) === expected);
    const endValue = entry?.samples.at(-1)?.value;
    if (endValue === undefined || endValue < criterion.minimumValue) {
      fail(
        label + " required activity " + expected + " must be at least "
          + criterion.minimumValue + " at the frozen window end",
      );
    }
	}
}
