import { immutableJourneyContract } from "../../scripts/switchable-baseline/critical-journey-contract";

export const immutableGateEvidenceKinds = {
  "0": ["capability-baseline", "journey-baseline", "telemetry-baseline"],
  "1": [
    "availability-slo",
    "delivery-slo",
    "authentication-slo",
    "restore-drill",
    "security",
  ],
  "2": ["e2e", "interop", "security", "chaos"],
  "3": ["e2e", "chaos", "accessibility", "performance", "device"],
  "4": ["e2e", "metric", "authorization", "audit"],
  "5": [
    "hosted-pilot",
    "self-hosted-pilot",
    "federated-pilot",
    "eight-week-retention",
    "restore-drill",
    "extension-isolation",
    "views-adoption",
  ],
} as const;

export const immutableJourneyEvidenceKinds = Object.fromEntries(
  Object.entries(immutableJourneyContract).map(([id, journey]) => [
    id,
    journey.requiredKinds,
  ]),
) as {
  [Id in keyof typeof immutableJourneyContract]:
    (typeof immutableJourneyContract)[Id]["requiredKinds"];
};

export type ProgramGateId = keyof typeof immutableGateEvidenceKinds;
export type ProgramReadiness = "not-ready" | "ready";
export type JourneyEvidenceStatus = "missing" | "partial" | "complete";

export interface ScenarioBinding {
  scenarioId: string;
  client: string;
  topology: string;
  kind: string;
}

export interface ProgramEvidenceRecord {
  kind: string;
  status: "partial" | "complete";
  reference: Record<string, unknown>;
}

function sorted(values: readonly string[]): string[] {
  return [...values].sort();
}

export function requireExactImmutableKinds(
  actual: readonly string[],
  expected: readonly string[],
  label: string,
): void {
  if (
    new Set(actual).size !== actual.length
    || JSON.stringify(sorted(actual)) !== JSON.stringify(sorted(expected))
  ) {
    throw new Error(`${label} requiredKinds must match the immutable program contract`);
  }
}

export function requireImmutableGateKinds(
  gateId: string,
  actual: readonly string[],
): asserts gateId is ProgramGateId {
  const expected = immutableGateEvidenceKinds[gateId as ProgramGateId];
  if (!expected) throw new Error(`unknown switchable-alternative gate ${gateId}`);
  requireExactImmutableKinds(actual, expected, `gate ${gateId}`);
}

export function requireImmutableJourneyKinds(
  journeyId: string,
  actual: readonly string[],
): void {
  const expected = immutableJourneyEvidenceKinds[
    journeyId as keyof typeof immutableJourneyEvidenceKinds
  ];
  if (!expected) throw new Error(`unknown switchable-alternative journey ${journeyId}`);
  requireExactImmutableKinds(actual, expected, `journey ${journeyId}`);
}

export function deriveGateReadiness(
  requiredKinds: readonly string[],
  completedKinds: ReadonlySet<string>,
): ProgramReadiness {
  return requiredKinds.every((kind) => completedKinds.has(kind))
    ? "ready"
    : "not-ready";
}

export function deriveJourneyEvidenceStatus(
  scenarioIds: ReadonlySet<string>,
  requiredKinds: readonly string[],
  records: readonly {
    scenarioId: string;
    kind: string;
    status: "partial" | "complete";
  }[],
): JourneyEvidenceStatus {
  if (records.length === 0) return "missing";
  const completed = new Set(
    records
      .filter(({ status }) => status === "complete")
      .map(({ scenarioId, kind }) => `${scenarioId}\0${kind}`),
  );
  for (const scenarioId of scenarioIds) {
    for (const kind of requiredKinds) {
      if (!completed.has(`${scenarioId}\0${kind}`)) return "partial";
    }
  }
  return "complete";
}

export function parseScenarioBinding(
  scenarioId: string,
  kind: string,
): ScenarioBinding {
  const segments = scenarioId.split("/");
  if (segments.length !== 4 || segments.some((segment) => segment.length === 0)) {
    throw new Error(`invalid critical-journey scenario scope ${scenarioId}`);
  }
  return {
    scenarioId,
    client: segments[2],
    topology: segments[3],
    kind,
  };
}

export function scenarioTestId(binding: ScenarioBinding): string {
  return ["switchable", ...binding.scenarioId.split("/"), binding.kind]
    .join("__")
    .replaceAll("-", "_");
}

export function requireExactScenarioReferenceBinding(
  reference: Record<string, unknown>,
  binding: ScenarioBinding,
): void {
  for (const [key, expected] of Object.entries(binding)) {
    if (reference[key] !== expected) {
      throw new Error(`scenario evidence reference.${key} must match its exact record scope`);
    }
  }
}

export function requireCompleteEvidenceReferencePolicy(
  gateId: ProgramGateId,
  record: ProgramEvidenceRecord,
  binding?: ScenarioBinding,
): void {
  if (record.status !== "complete") return;
  const type = record.reference.type;

  if (gateId === "0") {
    if (["capability-baseline", "telemetry-baseline"].includes(record.kind)) {
      if (type !== "artifact-manifest") {
        throw new Error(`complete Gate 0 ${record.kind} must use an artifact-manifest`);
      }
      return;
    }
    if (
      record.kind !== "journey-baseline"
      || type !== "journey-baseline-manifest"
    ) {
      throw new Error("complete Gate 0 journey-baseline must use a typed immutable manifest");
    }
    return;
  }

  if (gateId === "1" || gateId === "5") {
    throw new Error(
      `Gate ${gateId} evidence requires a verified kind-specific operational or pilot attestation`,
    );
  }

  if (!binding) {
    throw new Error(`complete Gate ${gateId} journey evidence requires an exact scenario scope`);
  }
  requireExactScenarioReferenceBinding(record.reference, binding);
  throw new Error(
    "journey evidence requires a verified passing-run or manual/live evidence attestation with a kind-specific contract",
  );
}

export function requireUniqueCompletedKinds(
  records: readonly ProgramEvidenceRecord[],
  label: string,
): void {
  const completed = records
    .filter(({ status }) => status === "complete")
    .map(({ kind }) => kind);
  if (new Set(completed).size !== completed.length) {
    throw new Error(`${label} must have at most one complete record per evidence kind`);
  }
}
