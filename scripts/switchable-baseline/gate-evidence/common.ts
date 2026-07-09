import { GATE_ZERO_FARO_SIGNAL_IDS } from "../model";

export type GateZeroArtifactEvidenceKind =
  | "capability-baseline"
  | "telemetry-baseline";

export type CapabilityArtifactRole =
  | "disco-target-contract"
  | "live-disco-export"
  | "capability-reconciliation";

export type TelemetryArtifactRole =
  | "prometheus-baseline"
  | "faro-browser-auth-bootstrap"
  | "faro-browser-message-ack-latency"
  | "faro-browser-session-lifecycle"
  | "faro-browser-reconnect-duration";

export interface EvidenceWindow {
  start: string;
  end: string;
}

export interface EvidenceRelease {
  serverCommit: string;
  webCommit: string;
}

export interface EvidenceDeploymentScope {
  job: string;
  environment: string;
  cluster: string;
  namespace: string;
  expectedReplicas: number;
  identityMetric: string;
  targetSignalId: string;
  identityLookbackSeconds: number;
}

export interface ArtifactManifestReference {
  type: "artifact-manifest";
  path: string;
  sha256: string;
}

export interface CapabilityArtifact {
  role: CapabilityArtifactRole;
  path: string;
  sha256: string;
  release: EvidenceRelease;
  window: EvidenceWindow;
}

export interface TelemetryArtifact {
  role: TelemetryArtifactRole;
  path: string;
  sha256: string;
  release: EvidenceRelease;
  window: EvidenceWindow;
}

export interface CapabilityArtifactManifest {
  schemaVersion: 1;
  evidenceKind: "capability-baseline";
  status: "complete";
  release: EvidenceRelease;
  window: EvidenceWindow;
  capturedAt: string;
  artifacts: CapabilityArtifact[];
}

export interface TelemetryArtifactManifest {
  schemaVersion: 1;
  evidenceKind: "telemetry-baseline";
  status: "complete";
  release: EvidenceRelease;
  capturedAt: string;
  window: EvidenceWindow;
  artifacts: TelemetryArtifact[];
}

export type GateZeroArtifactManifest =
  | CapabilityArtifactManifest
  | TelemetryArtifactManifest;

export type ValidatedGateZeroArtifactManifest = GateZeroArtifactManifest & {
  deploymentScope: EvidenceDeploymentScope;
  catalogSha256?: string;
  repositoryRoot: string;
  referencePath: string;
};

export const requiredRoles: Record<
  GateZeroArtifactEvidenceKind,
  readonly string[]
> = {
  "capability-baseline": [
    "disco-target-contract",
    "live-disco-export",
    "capability-reconciliation",
  ],
  "telemetry-baseline": [
    "prometheus-baseline",
    "faro-browser-auth-bootstrap",
    "faro-browser-message-ack-latency",
    "faro-browser-session-lifecycle",
    "faro-browser-reconnect-duration",
  ],
};

export const canonicalManifestPaths: Record<GateZeroArtifactEvidenceKind, string> = {
  "capability-baseline": "docs/evidence/gate-0/capability-baseline.manifest.json",
  "telemetry-baseline": "docs/evidence/gate-0/telemetry-baseline.manifest.json",
};

export const canonicalArtifactPaths: Record<
  GateZeroArtifactEvidenceKind,
  Record<string, string>
> = {
  "capability-baseline": {
    "disco-target-contract":
      "docs/evidence/gate-0/capability/disco-target-contract.json",
    "live-disco-export": "docs/evidence/gate-0/capability/live-disco-export.json",
    "capability-reconciliation":
      "docs/evidence/gate-0/capability/capability-reconciliation.json",
  },
  "telemetry-baseline": {
    "prometheus-baseline": "docs/evidence/gate-0/telemetry-baseline.json",
    "faro-browser-auth-bootstrap":
      "docs/evidence/gate-0/faro/browser-auth-bootstrap.json",
    "faro-browser-message-ack-latency":
      "docs/evidence/gate-0/faro/browser-message-ack-latency.json",
    "faro-browser-session-lifecycle":
      "docs/evidence/gate-0/faro/browser-session-lifecycle.json",
    "faro-browser-reconnect-duration":
      "docs/evidence/gate-0/faro/browser-reconnect-duration.json",
  },
};

export const canonicalGateZeroReviewPath =
  "docs/evidence/gate-0/telemetry-baseline.md";

export const canonicalGateZeroPaths = [
  ...Object.values(canonicalManifestPaths),
  ...Object.values(canonicalArtifactPaths).flatMap((paths) =>
    Object.values(paths)
  ),
  canonicalGateZeroReviewPath,
].sort();

export const expectedFaroSignalIds = GATE_ZERO_FARO_SIGNAL_IDS;

export const faroRoleSignalIds: Record<
  Exclude<TelemetryArtifactRole, "prometheus-baseline">,
  string
> = {
  "faro-browser-auth-bootstrap": "browser-auth-bootstrap",
  "faro-browser-message-ack-latency": "browser-message-ack-latency",
  "faro-browser-session-lifecycle": "browser-session-lifecycle",
  "faro-browser-reconnect-duration": "browser-reconnect-duration",
};

const commitPattern = /^[0-9a-f]{40}$/;
const sha256Pattern = /^[0-9a-f]{64}$/;
export const boundedLabelPattern =
  /^[a-z0-9](?:[a-z0-9._-]{0,62}[a-z0-9])?$/;
export const metricPattern = /^[a-zA-Z_:][a-zA-Z0-9_:]*$/;
export const kebabPattern = /^[a-z][a-z0-9-]*$/;
const utcInstantPattern = new RegExp(
  "^([0-9]{4})-([0-9]{2})-([0-9]{2})T"
    + "([0-9]{2}):([0-9]{2}):([0-9]{2})([.][0-9]+)?Z$",
);

export function fail(message: string): never {
  throw new Error("gate evidence: " + message);
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function requireRecord(
  value: unknown,
  label: string,
): Record<string, unknown> {
  if (!isRecord(value)) fail(label + " must be an object");
  return value;
}

export function requireExactKeys(
  value: Record<string, unknown>,
  expected: readonly string[],
  label: string,
): void {
  const actual = Object.keys(value).sort();
  const sortedExpected = [...expected].sort();
  if (JSON.stringify(actual) !== JSON.stringify(sortedExpected)) {
    fail(label + " must contain exactly: " + sortedExpected.join(", "));
  }
}

export function requireString(
  value: Record<string, unknown>,
  key: string,
  label: string,
): string {
  const entry = value[key];
  if (typeof entry !== "string" || entry.length === 0) {
    fail(label + "." + key + " must be a non-empty string");
  }
  return entry;
}

export function requireInteger(
  value: unknown,
  label: string,
  minimum = 0,
): number {
  if (!Number.isSafeInteger(value) || (value as number) < minimum) {
    fail(label + " must be a safe integer greater than or equal to " + minimum);
  }
  return value as number;
}

export function requireFiniteNumber(
  value: unknown,
  label: string,
  minimum = 0,
): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < minimum) {
    fail(label + " must be a finite number greater than or equal to " + minimum);
  }
  return value;
}

export function requireCommit(value: unknown, label: string): string {
  if (typeof value !== "string" || !commitPattern.test(value)) {
    fail(label + " must be a full 40-character lowercase Git commit SHA");
  }
  return value;
}

export function parseRelease(value: unknown, label: string): EvidenceRelease {
  const release = requireRecord(value, label);
  requireExactKeys(release, ["serverCommit", "webCommit"], label);
  return {
    serverCommit: requireCommit(release.serverCommit, label + ".serverCommit"),
    webCommit: requireCommit(release.webCommit, label + ".webCommit"),
  };
}

export function releasesEqual(
  left: EvidenceRelease,
  right: EvidenceRelease,
): boolean {
  return left.serverCommit === right.serverCommit
    && left.webCommit === right.webCommit;
}

export function requireSha256(value: unknown, label: string): string {
  if (typeof value !== "string" || !sha256Pattern.test(value)) {
    fail(label + " must be a lowercase SHA-256 digest");
  }
  return value;
}

export function requireStringArray(
  value: unknown,
  label: string,
): string[] {
  if (
    !Array.isArray(value)
    || value.some((entry) => typeof entry !== "string" || entry.length === 0)
  ) {
    fail(label + " must be an array of non-empty strings");
  }
  const entries = value as string[];
  if (new Set(entries).size !== entries.length) {
    fail(label + " values must be unique");
  }
  return entries;
}

export function requireSortedExactStrings(
  value: unknown,
  expected: readonly string[],
  label: string,
): string[] {
  const actual = requireStringArray(value, label);
  const sortedExpected = [...expected].sort();
  if (JSON.stringify(actual) !== JSON.stringify(sortedExpected)) {
    fail(label + " must equal the sorted closed set: " + sortedExpected.join(", "));
  }
  return actual;
}

export function parseWindow(value: unknown, label: string): EvidenceWindow {
  const record = requireRecord(value, label);
  requireExactKeys(record, ["start", "end"], label);
  const start = requireUtcInstant(requireString(record, "start", label), label + ".start");
  const end = requireUtcInstant(requireString(record, "end", label), label + ".end");
  const startMs = Date.parse(start);
  const endMs = Date.parse(end);
  if (!Number.isFinite(startMs) || !Number.isFinite(endMs) || endMs <= startMs) {
    fail(label + ".end must be later than " + label + ".start");
  }
  return { start, end };
}

export function requireUtcInstant(value: unknown, label: string): string {
  if (typeof value !== "string") {
    fail(label + " must be an RFC 3339 UTC instant");
  }
  const match = value.match(utcInstantPattern);
  const epochMilliseconds = Date.parse(value);
  if (!match || !Number.isFinite(epochMilliseconds)) {
    fail(label + " must be an RFC 3339 UTC instant");
  }
  const [, year, month, day, hour, minute, second] = match;
  const parsed = new Date(epochMilliseconds);
  if (
    parsed.getUTCFullYear() !== Number(year)
    || parsed.getUTCMonth() + 1 !== Number(month)
    || parsed.getUTCDate() !== Number(day)
    || parsed.getUTCHours() !== Number(hour)
    || parsed.getUTCMinutes() !== Number(minute)
    || parsed.getUTCSeconds() !== Number(second)
  ) {
    fail(label + " must be an RFC 3339 UTC instant");
  }
  return value;
}

export function sameWindow(
  left: EvidenceWindow,
  right: EvidenceWindow,
): boolean {
  return left.start === right.start && left.end === right.end;
}

export function scopesEqual(
  left: EvidenceDeploymentScope,
  right: EvidenceDeploymentScope,
): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

export function parseDeploymentScope(
  value: unknown,
  label: string,
): EvidenceDeploymentScope {
  const scope = requireRecord(value, label);
  requireExactKeys(scope, [
    "job",
    "environment",
    "cluster",
    "namespace",
    "expectedReplicas",
    "identityMetric",
    "targetSignalId",
    "identityLookbackSeconds",
  ], label);
  const job = requireString(scope, "job", label);
  const environment = requireString(scope, "environment", label);
  const cluster = requireString(scope, "cluster", label);
  const namespace = requireString(scope, "namespace", label);
  for (const [key, entry] of Object.entries({
    job,
    environment,
    cluster,
    namespace,
  })) {
    if (entry === "unknown" || !boundedLabelPattern.test(entry)) {
      fail(label + "." + key + " must be a bounded lowercase deployment label");
    }
  }
  const identityMetric = requireString(scope, "identityMetric", label);
  if (!metricPattern.test(identityMetric)) {
    fail(label + ".identityMetric must be a metric name");
  }
  const targetSignalId = requireString(scope, "targetSignalId", label);
  if (!kebabPattern.test(targetSignalId)) {
    fail(label + ".targetSignalId must be kebab-case");
  }
  const expectedReplicas = requireInteger(
    scope.expectedReplicas,
    label + ".expectedReplicas",
    1,
  );
  if (expectedReplicas > 10_000) {
    fail(label + ".expectedReplicas must be no greater than 10000");
  }
  const identityLookbackSeconds = requireInteger(
    scope.identityLookbackSeconds,
    label + ".identityLookbackSeconds",
    1,
  );
  if (identityLookbackSeconds % 60 !== 0) {
    fail(label + ".identityLookbackSeconds must be a whole number of 60-second steps");
  }
  return {
    job,
    environment,
    cluster,
    namespace,
    expectedReplicas,
    identityMetric,
    targetSignalId,
    identityLookbackSeconds,
  };
}
