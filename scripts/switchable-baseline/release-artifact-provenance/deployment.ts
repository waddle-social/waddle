import {
  requireExactKeys,
  requireInteger,
  requireRecord,
  requireString,
} from "../gate-evidence/common";
import type { ReplicaProvenance } from "../replica-provenance";
import { parseObservedArtifactDigests } from "./artifacts";
import type {
  ObservedFluxObject,
  ObservedReleaseDeployment,
  ReleaseArtifacts,
} from "./model";
import {
  exactLiteral,
  requireBoundedLabel,
  requireNonPlaceholderSha256,
  requireOciDigest,
} from "./validation";

function parseFluxObject(
  value: unknown,
  expected: {
    apiVersion: string;
    kind: string;
    name: string;
    namespace: string;
  },
  label: string,
  extraDigestKey?: "artifactDigest" | "sourceArtifactDigest",
): ObservedFluxObject & Record<string, unknown> {
  const object = requireRecord(value, label);
  const keys = [
    "apiVersion",
    "kind",
    "name",
    "namespace",
    "uidSha256",
    "generation",
    "observedGeneration",
    "ready",
  ];
  if (extraDigestKey) keys.push(extraDigestKey);
  requireExactKeys(object, keys, label);
  for (const [key, expectedValue] of Object.entries(expected)) {
    exactLiteral(object[key], expectedValue, `${label}.${key}`);
  }
  if (object.ready !== true) throw new Error(`${label}.ready must be true`);
  const generation = requireInteger(object.generation, `${label}.generation`, 1);
  const observedGeneration = requireInteger(
    object.observedGeneration,
    `${label}.observedGeneration`,
    1,
  );
  if (observedGeneration !== generation) {
    throw new Error(`${label} must have reconciled its observed generation`);
  }
  const result: ObservedFluxObject & Record<string, unknown> = {
    ...expected,
    uidSha256: requireNonPlaceholderSha256(
      object.uidSha256,
      `${label}.uidSha256`,
    ),
    generation,
    observedGeneration,
    ready: true,
  };
  if (extraDigestKey) {
    result[extraDigestKey] = requireOciDigest(
      object[extraDigestKey],
      `${label}.${extraDigestKey}`,
    );
  }
  return result;
}

function parseObservedKubernetesDeployment(
  value: unknown,
  replica: Extract<ReplicaProvenance, { kind: "kubernetes-deployment" }>,
): Extract<ObservedReleaseDeployment, { kind: "kubernetes-flux" }>["deployment"] {
  const label = "release artifact provenance.observedDeployment.deployment";
  const deployment = requireRecord(value, label);
  requireExactKeys(
    deployment,
    [
      "apiVersion",
      "name",
      "namespace",
      "uidSha256",
      "generation",
      "observedGeneration",
      "specReplicas",
      "updatedReplicas",
      "readyReplicas",
      "availableReplicas",
      "configSha256",
    ],
    label,
  );
  exactLiteral(deployment.apiVersion, "apps/v1", `${label}.apiVersion`);
  const parsed = {
    apiVersion: "apps/v1" as const,
    name: requireBoundedLabel(deployment.name, `${label}.name`),
    namespace: requireBoundedLabel(deployment.namespace, `${label}.namespace`),
    uidSha256: requireNonPlaceholderSha256(
      deployment.uidSha256,
      `${label}.uidSha256`,
    ),
    generation: requireInteger(deployment.generation, `${label}.generation`, 1),
    observedGeneration: requireInteger(
      deployment.observedGeneration,
      `${label}.observedGeneration`,
      1,
    ),
    specReplicas: requireInteger(
      deployment.specReplicas,
      `${label}.specReplicas`,
      1,
    ),
    updatedReplicas: requireInteger(
      deployment.updatedReplicas,
      `${label}.updatedReplicas`,
      1,
    ),
    readyReplicas: requireInteger(
      deployment.readyReplicas,
      `${label}.readyReplicas`,
      1,
    ),
    availableReplicas: requireInteger(
      deployment.availableReplicas,
      `${label}.availableReplicas`,
      1,
    ),
    configSha256: requireNonPlaceholderSha256(
      deployment.configSha256,
      `${label}.configSha256`,
    ),
  };
  const expected = replica.deployment;
  if (
    parsed.name !== expected.name
    || parsed.namespace !== expected.namespace
    || parsed.uidSha256 !== expected.uidSha256
    || parsed.generation !== expected.generation
    || parsed.observedGeneration !== expected.observedGeneration
    || parsed.specReplicas !== expected.specReplicas
    || parsed.configSha256 !== expected.configSha256
    || parsed.updatedReplicas !== expected.specReplicas
    || parsed.readyReplicas !== expected.specReplicas
    || parsed.availableReplicas !== expected.specReplicas
  ) {
    throw new Error(`${label} does not match the attested live replica provenance`);
  }
  return parsed;
}

export function parseObservedDeployment(
  value: unknown,
  artifacts: ReleaseArtifacts,
  replicaProvenance: ReplicaProvenance,
): ObservedReleaseDeployment {
  const label = "release artifact provenance.observedDeployment";
  const observed = requireRecord(value, label);
  requireExactKeys(
    observed,
    [
      "kind",
      "deployment",
      "artifactDigests",
      ...(observed.kind === "kubernetes-flux" ? ["flux"] : []),
    ],
    label,
  );
  const artifactDigests = parseObservedArtifactDigests(
    observed.artifactDigests,
    artifacts,
  );
  if (observed.kind === "kubernetes-flux") {
    if (replicaProvenance.kind !== "kubernetes-deployment") {
      throw new Error(`${label}.kind does not match replica provenance`);
    }
    const deployment = parseObservedKubernetesDeployment(
      observed.deployment,
      replicaProvenance,
    );
    const flux = requireRecord(observed.flux, `${label}.flux`);
    requireExactKeys(
      flux,
      ["gitOpsSource", "kustomization", "chartSource", "helmRelease"],
      `${label}.flux`,
    );
    const gitOpsSource = parseFluxObject(
      flux.gitOpsSource,
      {
        apiVersion: "source.toolkit.fluxcd.io/v1",
        kind: "OCIRepository",
        name: "waddle-server",
        namespace: "flux-system",
      },
      `${label}.flux.gitOpsSource`,
      "artifactDigest",
    ) as ObservedFluxObject & { artifactDigest: string };
    const kustomization = parseFluxObject(
      flux.kustomization,
      {
        apiVersion: "kustomize.toolkit.fluxcd.io/v1",
        kind: "Kustomization",
        name: "infra-waddle-server",
        namespace: "flux-system",
      },
      `${label}.flux.kustomization`,
      "sourceArtifactDigest",
    ) as ObservedFluxObject & { sourceArtifactDigest: string };
    const chartSource = parseFluxObject(
      flux.chartSource,
      {
        apiVersion: "source.toolkit.fluxcd.io/v1",
        kind: "OCIRepository",
        name: "waddle-server-chart",
        namespace: "waddle",
      },
      `${label}.flux.chartSource`,
      "artifactDigest",
    ) as ObservedFluxObject & { artifactDigest: string };
    const helmRelease = parseFluxObject(
      flux.helmRelease,
      {
        apiVersion: "helm.toolkit.fluxcd.io/v2",
        kind: "HelmRelease",
        name: "waddle-server",
        namespace: "waddle",
      },
      `${label}.flux.helmRelease`,
    ) as ObservedFluxObject;
    if (
      gitOpsSource.artifactDigest !== artifacts.gitOps.digest
      || kustomization.sourceArtifactDigest !== artifacts.gitOps.digest
      || chartSource.artifactDigest !== artifacts.helmChart.digest
    ) {
      throw new Error(`${label}.flux does not match the published release artifacts`);
    }
    return {
      kind: "kubernetes-flux",
      deployment,
      flux: { gitOpsSource, kustomization, chartSource, helmRelease },
      artifactDigests,
    };
  }
  if (observed.kind === "self-hosted") {
    if (replicaProvenance.kind !== "self-hosted-config") {
      throw new Error(`${label}.kind does not match replica provenance`);
    }
    const deployment = requireRecord(observed.deployment, `${label}.deployment`);
    requireExactKeys(
      deployment,
      ["replicas", "configSha256", "operatorArtifactSha256"],
      `${label}.deployment`,
    );
    const parsed = {
      replicas: requireInteger(
        deployment.replicas,
        `${label}.deployment.replicas`,
        1,
      ),
      configSha256: requireNonPlaceholderSha256(
        deployment.configSha256,
        `${label}.deployment.configSha256`,
      ),
      operatorArtifactSha256: requireNonPlaceholderSha256(
        deployment.operatorArtifactSha256,
        `${label}.deployment.operatorArtifactSha256`,
      ),
    };
    if (JSON.stringify(parsed) !== JSON.stringify(replicaProvenance.deployment)) {
      throw new Error(`${label}.deployment does not match replica provenance`);
    }
    return { kind: "self-hosted", deployment: parsed, artifactDigests };
  }
  requireString(observed, "kind", label);
  throw new Error(`${label}.kind is unsupported`);
}
