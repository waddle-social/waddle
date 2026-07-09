import { afterEach } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import {
  validateArtifactManifestReference,
  type ArtifactManifestReference,
  type EvidenceDeploymentScope,
  type EvidenceWindow,
  type GateZeroArtifactEvidenceKind,
  type ValidatedGateZeroArtifactManifest,
} from "./gate-evidence";
import { parseBaselineCatalog } from "../../scripts/switchable-baseline/catalog";
import {
  materializeFaroQueryPlan,
} from "../../scripts/switchable-baseline/faro";
import {
  MANUAL_FARO_NOTE,
  PARTIAL_EVIDENCE_CONCLUSION,
  renderEvidenceMarkdown,
} from "../../scripts/switchable-baseline/evidence";
import {
  canonicalArtifactPaths,
  canonicalManifestPaths,
} from "../../scripts/switchable-baseline/gate-evidence/common";
import {
  FARO_WEB_SOURCE_PATHS,
  GATE_ZERO_SOURCE_PATHS,
} from "../../scripts/switchable-baseline/source-contract";
import type { ReplicaProvenance } from "../../scripts/switchable-baseline/replica-provenance";
import { releaseArtifactProvenanceFixture } from "./release-artifact-provenance";

export const repositoryRoot = resolve(import.meta.dir, "../..");
export const serverCommit = "0123456789abcdef0123456789abcdef01234567";
export const webCommit = "1111111111111111111111111111111111111111";
export const otherServerCommit = "89abcdef0123456789abcdef0123456789abcdef";
export const otherWebCommit = "2222222222222222222222222222222222222222";
export const release = { serverCommit, webCommit };
export const window: EvidenceWindow = {
  start: "2026-07-10T09:00:00Z",
  end: "2026-07-10T10:00:00Z",
};
export const capturedAt = "2026-07-10T10:05:00Z";
export const capabilityRoles = [
  "disco-target-contract",
  "live-disco-export",
  "capability-reconciliation",
] as const;
export const telemetryRoles = [
  "prometheus-baseline",
  "faro-browser-auth-bootstrap",
  "faro-browser-message-ack-latency",
  "faro-browser-session-lifecycle",
  "faro-browser-reconnect-duration",
] as const;

const temporaryRoots: string[] = [];

export function trackTemporaryRoot(path: string): string {
  temporaryRoots.push(path);
  return path;
}

afterEach(async () => {
  await Promise.all(temporaryRoots.splice(0).map((path) => rm(path, {
    recursive: true,
    force: true,
  })));
});

export function sha256(value: string | Uint8Array): string {
  return createHash("sha256").update(value).digest("hex");
}

export function serialize(value: unknown): string {
  return JSON.stringify(value, null, 2) + "\n";
}

export async function fixtureRoot(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "waddle-gate-evidence-"));
  temporaryRoots.push(root);
  await mkdir(resolve(root, "docs/evidence"), { recursive: true });
  return root;
}

export async function writeFixtureFile(
  root: string,
  path: string,
  contents: string,
): Promise<string> {
  const absolutePath = resolve(root, path);
  await mkdir(dirname(absolutePath), { recursive: true });
  await Bun.write(absolutePath, contents);
  return sha256(contents);
}

export type FixtureArtifact = {
  role: string;
  path: string;
  sha256: string;
  release: { serverCommit: string; webCommit: string };
  window: EvidenceWindow;
};

export type Fixture = {
  root: string;
  evidenceKind: GateZeroArtifactEvidenceKind;
  release: { serverCommit: string; webCommit: string };
  artifacts: FixtureArtifact[];
  contents: Map<string, Record<string, unknown>>;
  reference: ArtifactManifestReference;
};

export async function installSources(root: string): Promise<{
  catalog: Record<string, unknown>;
  catalogSha256: string;
  capabilityManifest: Record<string, unknown>;
  capabilityManifestSha256: string;
  targetContract: Record<string, unknown>;
  targetContractSha256: string;
}> {
  for (const path of GATE_ZERO_SOURCE_PATHS) {
    await writeFixtureFile(root, path, await Bun.file(resolve(repositoryRoot, path)).text());
  }
  const sourceCatalog = await Bun.file(
    resolve(repositoryRoot, "docs/observability/switchable-baseline-signals.json"),
  ).json() as Record<string, unknown>;
  const sourceDeploymentScope = sourceCatalog.deploymentScope as Record<string, unknown>;
  const catalog = {
    ...sourceCatalog,
    deploymentScope: {
      ...sourceDeploymentScope,
      maximumRangeLookbackSeconds: 3600,
    },
  };
  const catalogContents = serialize(catalog);
  const catalogSha256 = await writeFixtureFile(
    root,
    "docs/observability/switchable-baseline-signals.json",
    catalogContents,
  );

  const capabilityManifestSource = await Bun.file(
    resolve(repositoryRoot, "server/capabilities.toml"),
  ).text();
  const capabilityManifestSha256 = await writeFixtureFile(
    root,
    "server/capabilities.toml",
    capabilityManifestSource,
  );
  const capabilityManifest = Bun.TOML.parse(capabilityManifestSource) as Record<string, unknown>;
  const targetContractSource = await Bun.file(
    resolve(repositoryRoot, "server/disco-target-contract.json"),
  ).text();
  const targetContractSha256 = await writeFixtureFile(
    root,
    "server/disco-target-contract.json",
    targetContractSource,
  );
  return {
    catalog,
    catalogSha256,
    capabilityManifest,
    capabilityManifestSha256,
    targetContract: JSON.parse(targetContractSource) as Record<string, unknown>,
    targetContractSha256,
  };
}

export function scopeFor(catalog: Record<string, unknown>): EvidenceDeploymentScope {
  const catalogScope = catalog.deploymentScope as Record<string, unknown>;
  return {
    job: "waddle-server",
    environment: "production",
    cluster: "waddle-cloud",
    namespace: "waddle",
    expectedReplicas: 2,
    identityMetric: catalogScope.identityMetric as string,
    targetSignalId: catalogScope.targetSignalId as string,
    identityLookbackSeconds: catalogScope.maximumRangeLookbackSeconds as number,
  };
}

export function combinations(attributes: Record<string, string[]>): Record<string, string>[] {
  let result: Record<string, string>[] = [{}];
  for (const key of Object.keys(attributes).sort()) {
    result = result.flatMap((existing) =>
      [...attributes[key]].sort().map((value) => ({ ...existing, [key]: value }))
    );
  }
  return result;
}

export function fixedSamples(
  value: number,
  lookbackSeconds = 0,
): Array<{ timestamp: number; value: number }> {
  const samples: Array<{ timestamp: number; value: number }> = [];
  for (
    let timestamp = Date.parse(window.start) / 1_000 - lookbackSeconds;
    timestamp <= Date.parse(window.end) / 1_000;
    timestamp += 60
  ) {
    samples.push({ timestamp, value });
  }
  return samples;
}

export function prometheusArtifact(
  catalog: Record<string, unknown>,
  catalogSha256: string,
  fixtureServerCommit: string,
): Record<string, unknown> {
  const scope = scopeFor(catalog);
  const selector = "{job=\"" + scope.job
    + "\",deployment_environment=\"" + scope.environment
    + "\",cluster=\"" + scope.cluster
    + "\",namespace=\"" + scope.namespace + "\"}";
  const catalogSignals = catalog.signals as Array<Record<string, unknown>>;
  const signals = catalogSignals
    .filter(({ source, collection }) => source === "prometheus" && collection === "automated")
    .sort((left, right) => String(left.id).localeCompare(String(right.id)))
    .map((signal) => {
      const id = signal.id as string;
      const attributes = Object.fromEntries(
        Object.entries(signal.attributes as Record<string, string[]>).map(([key, values]) => [
          key,
          values.map((value) => ({
            "{{commit}}": fixtureServerCommit,
            "{{cluster}}": scope.cluster,
            "{{environment}}": scope.environment,
          })[value] ?? value),
        ]),
      );
      const series = combinations(attributes).map((entry) => {
        let value = 1;
        if (id === scope.targetSignalId) value = scope.expectedReplicas;
        if (id === "loss-corruption-safety") value = 0;
        if (
          id === "live-delivery-channel-outcomes"
          && entry.outcome !== "delivered"
        ) value = 0;
        const samples = fixedSamples(
          value,
          Number(signal.collectionLookbackSeconds ?? 0),
        );
        return {
          attributes: entry,
          samples,
          canonicalEndSample: samples.at(-1),
        };
      });
      return {
        id,
        query: String(signal.query).replaceAll("{{scope}}", selector),
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
    });

  return {
    schemaVersion: 1,
    artifactRole: "prometheus-baseline",
    evidenceKind: "gate-0-switchable-baseline",
    milestone: "switchable-alternative",
    gate: 0,
    status: "partial",
    gateReadiness: "not-ready",
    serverCommit: fixtureServerCommit,
    deploymentScope: scope,
    catalog: {
      path: "docs/observability/switchable-baseline-signals.json",
      sha256: catalogSha256,
      schemaVersion: 1,
    },
    collectionWindow: {
      ...window,
      durationMinutes: 60,
      minimumDurationMinutes: 60,
      stepSeconds: 60,
    },
    automatedPrometheus: {
      status: "collected",
      signals,
    },
    manualFaro: {
      status: "required",
      signalIds: [
        "browser-auth-bootstrap",
        "browser-message-ack-latency",
        "browser-reconnect-duration",
        "browser-session-lifecycle",
      ],
      note: MANUAL_FARO_NOTE,
    },
    conclusion: PARTIAL_EVIDENCE_CONCLUSION,
  };
}

export function faroSeries(role: string): Record<string, unknown>[] {
  if (role === "faro-browser-auth-bootstrap") {
    return ["expired", "failed", "ready", "signed_out"].map((outcome, index) => ({
      attributes: { outcome },
      count: index === 2 ? 4 : 0,
      durationMs: index === 2
        ? { count: 4, p50: 120, p95: 220 }
        : { count: 0, p50: null, p95: null },
    }));
  }
  if (role === "faro-browser-message-ack-latency") {
    return ["dm", "room"].map((kind) => ({
      attributes: { kind },
      count: 2,
      latencyMs: { p50: 20, p95: 60 },
    }));
  }
  if (role === "faro-browser-session-lifecycle") {
    return ["fresh", "resumed"].map((type) => ({
      attributes: { type },
      count: 2,
    }));
  }
  return [{
    attributes: {},
    count: 3,
    durationMs: { p50: 300, p95: 700 },
  }];
}

export function faroArtifact(
  role: Exclude<(typeof telemetryRoles)[number], "prometheus-baseline">,
  catalog: Record<string, unknown>,
  fixtureRelease: { serverCommit: string; webCommit: string },
): Record<string, unknown> {
  const signalId = role.slice("faro-".length);
  const signal = (catalog.signals as Array<Record<string, unknown>>)
    .find(({ id }) => id === signalId);
  if (!signal) throw new Error("missing Faro fixture signal " + signalId);
  const parsedSignal = parseBaselineCatalog(catalog).signals.find(({ id }) => id === signalId);
  if (!parsedSignal) throw new Error("missing parsed Faro fixture signal " + signalId);
  const dimensions = Object.fromEntries(
    Object.entries(signal.attributes as Record<string, string[]>)
      .map(([key, values]) => [key, [...values].sort()]),
  );
  const series = faroSeries(role);
  return {
    schemaVersion: 1,
    evidenceKind: "gate-0-faro-aggregate",
    role,
    signalId,
    release: fixtureRelease,
    window,
    scope: {
      sourceId: "waddle-chat",
      deploymentEnvironment: "production",
      release: fixtureRelease.webCommit,
      cluster: "waddle-cloud",
      namespace: "waddle",
    },
    source: {
      sourceId: "waddle-chat",
      query: materializeFaroQueryPlan(parsedSignal, {
        webCommit: fixtureRelease.webCommit,
        deploymentEnvironment: "production",
        cluster: "waddle-cloud",
        namespace: "waddle",
        window,
      }),
      rawSha256: "a".repeat(64),
      rowCount: series.length,
    },
    dimensions,
    series,
  };
}

export function capabilityArtifacts(
  sources: Awaited<ReturnType<typeof installSources>>,
  fixtureServerCommit: string,
): Map<string, Record<string, unknown>> {
  const capabilities = (sources.capabilityManifest.capability as Array<Record<string, unknown>>)
    .map((capability) => {
      const targets = new Map<string, string[]>();
      for (const claimMap of [
        capability.advertised_features,
        capability.custom_namespaces ?? {},
      ] as Record<string, unknown>[]) {
        for (const [target, features] of Object.entries(claimMap)) {
          targets.set(target, [
            ...(targets.get(target) ?? []),
            ...(features as string[]),
          ].sort());
        }
      }
      return {
        id: capability.id as string,
        targets: [...targets].sort(([left], [right]) => left.localeCompare(right)),
      };
    })
    .sort((left, right) => left.id.localeCompare(right.id));
  const targetContractEntries = sources.targetContract.targets as Array<Record<string, unknown>>;
  const claimedFeaturesByTarget = new Map<string, Set<string>>();
  for (const { targets } of capabilities) {
    for (const [target, features] of targets) {
      const claimed = claimedFeaturesByTarget.get(target) ?? new Set<string>();
      features.forEach((feature) => claimed.add(feature));
      claimedFeaturesByTarget.set(target, claimed);
    }
  }
  const scope = scopeFor(sources.catalog);
  const live = {
    schemaVersion: 1,
    artifactRole: "live-disco-export",
    evidenceKind: "gate-0-capability-live-disco",
    status: "collected",
    serverCommit: fixtureServerCommit,
    capturedAt: "2026-07-10T09:55:00Z",
    window,
    deploymentScope: scope,
    targetContractSha256: sources.targetContractSha256,
    entities: targetContractEntries.map((target) => {
      const runtimeVariants = target.runtime_feature_variants as string[][];
      const optionalFeatures = target.independently_optional_features as string[];
      const claims = claimedFeaturesByTarget.get(target.slug as string) ?? new Set<string>();
      const baseFeatures = runtimeVariants.length > 0
        ? runtimeVariants.find((variant) =>
          [...claims].every((feature) =>
            variant.includes(feature) || optionalFeatures.includes(feature)
          )
        ) ?? runtimeVariants[0]
        : target.required_features as string[];
      return {
        target: target.slug,
        identities: (target.identities as Array<Record<string, unknown>>).map(
          ({ category, type }) => ({ category, type }),
        ),
        features: [
          ...baseFeatures,
          ...optionalFeatures,
        ].sort(),
      };
    }),
    skippedTargets: [],
  };
  const liveSha = sha256(serialize(live));
  const reconciliation = {
    schemaVersion: 1,
    artifactRole: "capability-reconciliation",
    evidenceKind: "gate-0-capability-reconciliation",
    status: "matched",
    serverCommit: fixtureServerCommit,
    capturedAt: "2026-07-10T09:58:00Z",
    deploymentScope: scope,
    targetContractSha256: sources.targetContractSha256,
    liveDiscoSha256: liveSha,
    capabilityManifest: {
      path: "server/capabilities.toml",
      sha256: sources.capabilityManifestSha256,
      schemaVersion: 1,
    },
    summary: {
      declaredCapabilityCount: capabilities.length,
      observedTargetCount: targetContractEntries.length,
      missingAdvertisedFeatures: [],
      unexpectedOfficialFeatures: [],
      capabilityMismatches: [],
    },
    checks: capabilities.map(({ id, targets }) => ({
      capabilityId: id,
      status: "matched",
      targets: targets.map(([target, features]) => ({
        target,
        declaredFeatures: [...features],
        observedFeatures: [...features],
      })),
    })),
  };
  return new Map([
    ["disco-target-contract", sources.targetContract],
    ["live-disco-export", live],
    ["capability-reconciliation", reconciliation],
  ]);
}

export async function writeRawManifest(
  root: string,
  evidenceKind: GateZeroArtifactEvidenceKind,
  manifest: unknown,
): Promise<ArtifactManifestReference> {
  const contents = serialize(manifest);
  const path = "docs/evidence/gate-0/" + evidenceKind + ".manifest.json";
  return {
    type: "artifact-manifest",
    path,
    sha256: await writeFixtureFile(root, path, contents),
  };
}

export async function writeFixtureManifest(fixture: Fixture): Promise<ArtifactManifestReference> {
  return writeRawManifest(fixture.root, fixture.evidenceKind, {
    schemaVersion: 1,
    evidenceKind: fixture.evidenceKind,
    status: "complete",
    release: fixture.release,
    window,
    capturedAt,
    artifacts: fixture.artifacts,
  });
}

export async function completeFixture(
  evidenceKind: GateZeroArtifactEvidenceKind,
  requestedRoot?: string,
  fixtureRelease = release,
): Promise<Fixture> {
  const root = requestedRoot ?? await fixtureRoot();
  const sources = await installSources(root);
  const contents = evidenceKind === "capability-baseline"
    ? capabilityArtifacts(sources, fixtureRelease.serverCommit)
    : new Map<string, Record<string, unknown>>([
      ["prometheus-baseline", prometheusArtifact(
        sources.catalog,
        sources.catalogSha256,
        fixtureRelease.serverCommit,
      )],
      ...telemetryRoles
        .filter((role) => role !== "prometheus-baseline")
        .map((role) => [role, faroArtifact(role, sources.catalog, fixtureRelease)] as const),
    ]);
  const roles = evidenceKind === "capability-baseline" ? capabilityRoles : telemetryRoles;
  const artifacts: FixtureArtifact[] = [];
  const paths: Record<string, string> = evidenceKind === "capability-baseline"
    ? {
      "disco-target-contract":
        "docs/evidence/gate-0/capability/disco-target-contract.json",
      "live-disco-export": "docs/evidence/gate-0/capability/live-disco-export.json",
      "capability-reconciliation":
        "docs/evidence/gate-0/capability/capability-reconciliation.json",
    }
    : {
      "prometheus-baseline": "docs/evidence/gate-0/telemetry-baseline.json",
      "faro-browser-auth-bootstrap": "docs/evidence/gate-0/faro/browser-auth-bootstrap.json",
      "faro-browser-message-ack-latency":
        "docs/evidence/gate-0/faro/browser-message-ack-latency.json",
      "faro-browser-session-lifecycle":
        "docs/evidence/gate-0/faro/browser-session-lifecycle.json",
      "faro-browser-reconnect-duration":
        "docs/evidence/gate-0/faro/browser-reconnect-duration.json",
    };
  for (const role of roles) {
    const path = paths[role];
    const artifactContents = serialize(contents.get(role));
    artifacts.push({
      role,
      path,
      sha256: await writeFixtureFile(root, path, artifactContents),
      release: fixtureRelease,
      window,
    });
  }
  if (evidenceKind === "telemetry-baseline") {
    const prometheus = artifacts.find(({ role }) => role === "prometheus-baseline");
    const prometheusContents = contents.get("prometheus-baseline");
    if (!prometheus || !prometheusContents) {
      throw new Error("missing Prometheus review fixture");
    }
    await writeFixtureFile(
      root,
      "docs/evidence/gate-0/telemetry-baseline.md",
      renderEvidenceMarkdown(prometheusContents as never, prometheus.sha256),
    );
  }
  const fixture: Fixture = {
    root,
    evidenceKind,
    release: fixtureRelease,
    artifacts,
    contents,
    reference: { type: "artifact-manifest", path: "", sha256: "" },
  };
  fixture.reference = await writeFixtureManifest(fixture);
  return fixture;
}

export async function rewriteArtifact(
  fixture: Fixture,
  role: string,
  mutate: (value: Record<string, unknown>) => void,
): Promise<void> {
  const value = structuredClone(fixture.contents.get(role));
  if (!value) throw new Error("missing fixture artifact " + role);
  mutate(value);
  fixture.contents.set(role, value);
  const artifact = fixture.artifacts.find((entry) => entry.role === role);
  if (!artifact) throw new Error("missing fixture manifest artifact " + role);
  artifact.sha256 = await writeFixtureFile(fixture.root, artifact.path, serialize(value));
  if (fixture.evidenceKind === "capability-baseline" && role === "live-disco-export") {
    const reconciliation = fixture.contents.get("capability-reconciliation");
    if (reconciliation) {
      reconciliation.liveDiscoSha256 = artifact.sha256;
      const reconciliationArtifact = fixture.artifacts.find(
        (entry) => entry.role === "capability-reconciliation",
      );
      if (reconciliationArtifact) {
        reconciliationArtifact.sha256 = await writeFixtureFile(
          fixture.root,
          reconciliationArtifact.path,
          serialize(reconciliation),
        );
      }
    }
  }
  fixture.reference = await writeFixtureManifest(fixture);
}

export async function validate(
  fixture: Fixture,
  sourceRelease = fixture.release,
): Promise<ValidatedGateZeroArtifactManifest> {
  return validateArtifactManifestReference(
    fixture.root,
    fixture.reference,
    fixture.evidenceKind,
    async (root, assertedCommit, repositoryPath) => {
      const expectedCommit = FARO_WEB_SOURCE_PATHS.includes(
        repositoryPath as (typeof FARO_WEB_SOURCE_PATHS)[number],
      )
        ? sourceRelease.webCommit
        : sourceRelease.serverCommit;
      if (assertedCommit !== expectedCommit) throw new Error("unknown source commit");
      return Bun.file(resolve(root, repositoryPath)).bytes();
    },
  );
}
