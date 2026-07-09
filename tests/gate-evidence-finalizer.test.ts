import { describe, expect, test } from "bun:test";
import { mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import type { ValidatedGateZeroArtifactManifest } from "./support/gate-evidence";
import { runSwitchableBaselineFinalizer } from "../scripts/finalize-switchable-baseline";
import { verifyGateZeroEvidencePackage } from "../scripts/switchable-baseline/finalize";
import {
  canonicalArtifactPaths,
  canonicalManifestPaths,
} from "../scripts/switchable-baseline/gate-evidence/common";
import { buildLiveCollectionSubject } from "../scripts/switchable-baseline/subject";
import { FARO_WEB_SOURCE_PATHS } from "../scripts/switchable-baseline/source-contract";
import type { ReplicaProvenance } from "../scripts/switchable-baseline/replica-provenance";
import { releaseArtifactProvenanceFixture } from "./support/release-artifact-provenance";
import {
  capabilityArtifacts,
  capturedAt,
  faroArtifact,
  fixtureRoot,
  installSources,
  prometheusArtifact,
  release,
  repositoryRoot,
  scopeFor,
  serialize,
  serverCommit,
  sha256,
  telemetryRoles,
  webCommit,
  window,
  writeFixtureFile,
} from "./support/gate-evidence-fixtures";

async function fixtureSourceAtCommit(
  root: string,
  assertedCommit: string,
  repositoryPath: string,
): Promise<Uint8Array> {
  const expectedCommit = FARO_WEB_SOURCE_PATHS.includes(
    repositoryPath as (typeof FARO_WEB_SOURCE_PATHS)[number],
  )
    ? webCommit
    : serverCommit;
  if (assertedCommit !== expectedCommit) throw new Error("unknown fixture commit");
  return Bun.file(resolve(root, repositoryPath)).bytes();
}

async function writeFaroFinalizerInputs(
  root: string,
  catalog: Record<string, unknown>,
): Promise<Record<Exclude<(typeof telemetryRoles)[number], "prometheus-baseline">, string>> {
  const paths = {} as Record<
    Exclude<(typeof telemetryRoles)[number], "prometheus-baseline">,
    string
  >;
  for (const role of telemetryRoles.slice(1)) {
    const path = `target/switchable-baseline-inputs/faro/${role.slice("faro-".length)}.json`;
    await writeFixtureFile(root, path, serialize(faroArtifact(role, catalog, release)));
    paths[role] = resolve(root, path);
  }
  return paths;
}

function allCliArguments(
  liveDiscoPath: string,
  prometheusPath: string,
  faroPaths: Record<Exclude<(typeof telemetryRoles)[number], "prometheus-baseline">, string>,
  subjectPath: string,
  bundlePath: string,
): string[] {
  return [
    "all",
    "--server-commit", serverCommit,
    "--web-commit", webCommit,
    "--start", window.start,
    "--end", window.end,
    "--captured-at", capturedAt,
    "--job", "waddle-server",
    "--deployment-environment", "production",
    "--cluster", "waddle-cloud",
    "--namespace", "waddle",
    "--expected-replicas", "2",
    "--identity-metric", "waddle_build_info",
    "--target-signal-id", "server-deployment-identity-targets",
    "--identity-lookback-seconds", "3600",
    "--live-disco", liveDiscoPath,
    "--prometheus", prometheusPath,
    "--faro-browser-auth-bootstrap", faroPaths["faro-browser-auth-bootstrap"],
    "--faro-browser-message-ack-latency",
    faroPaths["faro-browser-message-ack-latency"],
    "--faro-browser-session-lifecycle", faroPaths["faro-browser-session-lifecycle"],
    "--faro-browser-reconnect-duration", faroPaths["faro-browser-reconnect-duration"],
    "--collection-subject", subjectPath,
    "--attestation-bundle", bundlePath,
  ];
}

const acceptFixtureAttestation = async () => undefined;
const acceptFixtureReleaseArtifactProvenance = async () => undefined;

async function writeFixtureAttestation(
  root: string,
  catalog: Record<string, unknown>,
): Promise<{ subjectPath: string; bundlePath: string }> {
  const replicaProvenance: ReplicaProvenance = {
    schemaVersion: 1,
    kind: "kubernetes-deployment",
    deployment: {
      apiVersion: "apps/v1",
      name: "waddle-server",
      namespace: "waddle",
      uidSha256: sha256("apps/v1/waddle/waddle-server/uid"),
      generation: 42,
      observedGeneration: 42,
      specReplicas: 2,
      configSha256: sha256("waddle-server deployment generation 42"),
    },
  };
  const { path: subjectPath } = await buildLiveCollectionSubject({
    repositoryRoot: root,
    release,
    window,
    deploymentScope: scopeFor(catalog),
    replicaProvenance,
    releaseArtifactProvenance: releaseArtifactProvenanceFixture(
      release,
      replicaProvenance,
    ),
    environment: {
      GITHUB_SHA: serverCommit,
      GITHUB_REF: "refs/heads/main",
      GITHUB_WORKFLOW_REF:
        "waddle-social/waddle/.github/workflows/gate0-live-evidence.yml@refs/heads/main",
      WADDLE_CAPABILITY_ENDPOINT: "wss://xmpp.example.test/xmpp-websocket",
      GRAFANA_PROMETHEUS_URL: "https://prometheus.example.test/api/prom",
      PUBLIC_FARO_URL: "https://faro.example.test/collect/project",
      GRAFANA_FARO_QUERY_URL: "https://grafana.example.test/api/ds/query",
      GRAFANA_FARO_DATA_SOURCE_UID: "faro-query-source",
    },
  });
  const bundlePath = resolve(
    root,
    "target/switchable-baseline-inputs/attestation/live-collection.sigstore.json",
  );
  await Bun.write(bundlePath, "{}\n");
  return { subjectPath, bundlePath };
}

describe("Gate 0 evidence finalizer", () => {
  test("generates both canonical manifests and validates the sealed package", async () => {
    const root = await fixtureRoot();
    const sources = await installSources(root);
    const live = capabilityArtifacts(sources, serverCommit).get("live-disco-export");
    const livePath = resolve(
      root,
      "target/switchable-baseline-inputs/capability/live-disco-export.json",
    );
    await mkdir(dirname(livePath), { recursive: true });
    await Bun.write(livePath, serialize(live));

    const prometheusPath = resolve(
      root,
      "target/switchable-baseline-inputs/prometheus/telemetry-baseline.json",
    );
    await mkdir(dirname(prometheusPath), { recursive: true });
    await Bun.write(
      prometheusPath,
      serialize(prometheusArtifact(sources.catalog, sources.catalogSha256, serverCommit)),
    );
    const faroPaths = await writeFaroFinalizerInputs(root, sources.catalog);
    const attestation = await writeFixtureAttestation(root, sources.catalog);
    const allArguments = allCliArguments(
      livePath,
      prometheusPath,
      faroPaths,
      attestation.subjectPath,
      attestation.bundlePath,
    );
    const manifests = await runSwitchableBaselineFinalizer(
      allArguments,
      root,
      fixtureSourceAtCommit,
      acceptFixtureAttestation,
      acceptFixtureReleaseArtifactProvenance,
    ) as ValidatedGateZeroArtifactManifest[];
    expect(manifests.map(({ evidenceKind }) => evidenceKind).sort()).toEqual([
      "capability-baseline",
      "telemetry-baseline",
    ]);
    await expect(runSwitchableBaselineFinalizer(
      allArguments,
      root,
      fixtureSourceAtCommit,
      acceptFixtureAttestation,
      acceptFixtureReleaseArtifactProvenance,
    )).rejects.toThrow("refuses to replace an existing canonical generation");

    await expect(verifyGateZeroEvidencePackage(
      root,
      fixtureSourceAtCommit,
      acceptFixtureAttestation,
      acceptFixtureReleaseArtifactProvenance,
    )).resolves.toBeDefined();
    await expect(verifyGateZeroEvidencePackage(
      root,
      fixtureSourceAtCommit,
      acceptFixtureAttestation,
    )).rejects.toThrow("release-artifact-provenance blocker");
    const reconciliation = await Bun.file(resolve(
      root,
      canonicalArtifactPaths["capability-baseline"]["capability-reconciliation"],
    )).json() as Record<string, unknown>;
    expect((reconciliation.summary as Record<string, unknown>).capabilityMismatches).toEqual([]);
    expect(JSON.stringify(reconciliation)).not.toContain("@example");
  });

  test("rejects privacy-unsafe live input and leaves no partial canonical output", async () => {
    const root = await fixtureRoot();
    const sources = await installSources(root);
    const live = structuredClone(
      capabilityArtifacts(sources, serverCommit).get("live-disco-export"),
    ) as Record<string, unknown>;
    const first = (live.entities as Array<Record<string, unknown>>)[0];
    first.jid = "alice@example.test";
    const livePath = resolve(
      root,
      "target/switchable-baseline-inputs/capability/live-disco-export.json",
    );
    await mkdir(dirname(livePath), { recursive: true });
    await Bun.write(livePath, serialize(live));
		const prometheusPath = resolve(
			root,
			"target/switchable-baseline-inputs/prometheus/telemetry-baseline.json",
		);
		await mkdir(dirname(prometheusPath), { recursive: true });
		await Bun.write(
			prometheusPath,
			serialize(prometheusArtifact(sources.catalog, sources.catalogSha256, serverCommit)),
		);
		const faroPaths = await writeFaroFinalizerInputs(root, sources.catalog);
		const attestation = await writeFixtureAttestation(root, sources.catalog);
		await expect(runSwitchableBaselineFinalizer(
			allCliArguments(
				livePath,
				prometheusPath,
				faroPaths,
				attestation.subjectPath,
				attestation.bundlePath,
			),
			root,
			fixtureSourceAtCommit,
			acceptFixtureAttestation,
			acceptFixtureReleaseArtifactProvenance,
		)).rejects.toThrow("must contain exactly");
    for (const path of Object.values(canonicalArtifactPaths["capability-baseline"])) {
      expect(await Bun.file(resolve(root, path)).exists()).toBeFalse();
    }
    expect(await Bun.file(resolve(
      root,
      canonicalManifestPaths["capability-baseline"],
    )).exists()).toBeFalse();
  });

  test("detects output tampering and rejects malformed CLI arguments", async () => {
    const root = await fixtureRoot();
    const sources = await installSources(root);
    const live = capabilityArtifacts(sources, serverCommit).get("live-disco-export");
    const livePath = resolve(
      root,
      "target/switchable-baseline-inputs/capability/live-disco-export.json",
    );
    await mkdir(dirname(livePath), { recursive: true });
    await Bun.write(livePath, serialize(live));
		const prometheusPath = resolve(
			root,
			"target/switchable-baseline-inputs/prometheus/telemetry-baseline.json",
		);
		await mkdir(dirname(prometheusPath), { recursive: true });
		await Bun.write(
			prometheusPath,
			serialize(prometheusArtifact(sources.catalog, sources.catalogSha256, serverCommit)),
		);
		const faroPaths = await writeFaroFinalizerInputs(root, sources.catalog);
		const attestation = await writeFixtureAttestation(root, sources.catalog);
		await runSwitchableBaselineFinalizer(
			allCliArguments(
				livePath,
				prometheusPath,
				faroPaths,
				attestation.subjectPath,
				attestation.bundlePath,
			),
			root,
			fixtureSourceAtCommit,
			acceptFixtureAttestation,
			acceptFixtureReleaseArtifactProvenance,
		);
    await Bun.write(resolve(
      root,
      canonicalArtifactPaths["capability-baseline"]["live-disco-export"],
    ), "{}\n");
    await expect(verifyGateZeroEvidencePackage(
      root,
      fixtureSourceAtCommit,
      acceptFixtureAttestation,
      acceptFixtureReleaseArtifactProvenance,
    )).rejects.toThrow("SHA-256 does not match");
    await expect(runSwitchableBaselineFinalizer(["unknown"], root))
      .rejects.toThrow("mode must be");
    await expect(runSwitchableBaselineFinalizer([
      "all",
      "--server-commit", serverCommit,
      "--server-commit", serverCommit,
    ], root, fixtureSourceAtCommit)).rejects.toThrow("duplicate finalizer option");
  });
});
