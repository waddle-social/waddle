export const BUILD_IDENTITY_SCHEMA_VERSION = 1;
const BUILD_IDENTITY_MARKER_VERSION = "waddle-build-identity-v1";
export const FARO_SCOPE_LABEL_FIELDS = [
  "deploymentEnvironment",
  "cluster",
  "namespace",
  "sourceId",
] as const;
export const FARO_SCOPE_ENVIRONMENT_NAMES: Record<
  (typeof FARO_SCOPE_LABEL_FIELDS)[number],
  string
> = {
  deploymentEnvironment: "PUBLIC_FARO_DEPLOYMENT_ENVIRONMENT",
  cluster: "PUBLIC_FARO_CLUSTER",
  namespace: "PUBLIC_FARO_NAMESPACE",
  sourceId: "PUBLIC_FARO_SOURCE_ID",
};

const BOUNDED_BUILD_IDENTITY_LABEL =
  /^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/;
const FULL_BUILD_COMMIT = /^[0-9a-f]{40}$/;

export interface FaroBuildIdentityScope {
  deploymentEnvironment: string;
  cluster: string;
  namespace: string;
  sourceId: string;
  release: string;
}

export function requireBoundedBuildIdentityLabel(
  value: unknown,
  label: string,
): string {
  if (typeof value !== "string" || !BOUNDED_BUILD_IDENTITY_LABEL.test(value)) {
    throw new Error(`${label} must be a bounded lowercase deployment label`);
  }
  return value;
}

export function parseFaroBuildIdentityScope(
  value: unknown,
  expectedRelease: string,
  label: string,
): FaroBuildIdentityScope {
  requireFullBuildCommit(expectedRelease, `${label}.release`);
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const scope = value as Record<string, unknown>;
  const expectedKeys = [...FARO_SCOPE_LABEL_FIELDS, "release"].sort();
  if (JSON.stringify(Object.keys(scope).sort()) !== JSON.stringify(expectedKeys)) {
    throw new Error(`${label} contains unexpected fields`);
  }
  if (scope.release !== expectedRelease) {
    throw new Error(`${label}.release must match the full build commit`);
  }
  return {
    deploymentEnvironment: requireBoundedBuildIdentityLabel(
      scope.deploymentEnvironment,
      `${label}.deploymentEnvironment`,
    ),
    cluster: requireBoundedBuildIdentityLabel(scope.cluster, `${label}.cluster`),
    namespace: requireBoundedBuildIdentityLabel(
      scope.namespace,
      `${label}.namespace`,
    ),
    sourceId: requireBoundedBuildIdentityLabel(scope.sourceId, `${label}.sourceId`),
    release: expectedRelease,
  };
}

export function faroBuildIdentityMarker(
  commitSha: string,
  scope: FaroBuildIdentityScope,
): string {
  requireFullBuildCommit(commitSha, "Faro build identity commit");
  const parsed = parseFaroBuildIdentityScope(
    scope,
    commitSha,
    "Faro build identity scope",
  );
  return [
    BUILD_IDENTITY_MARKER_VERSION,
    commitSha,
    parsed.release,
    parsed.sourceId,
    parsed.deploymentEnvironment,
    parsed.cluster,
    parsed.namespace,
  ].join(":");
}

export function disabledBuildIdentityMarker(commitSha: string): string {
  if (commitSha !== "unknown") {
    requireFullBuildCommit(commitSha, "disabled build identity commit");
  }
  return `${BUILD_IDENTITY_MARKER_VERSION}:${commitSha}:faro-disabled`;
}

function requireFullBuildCommit(value: unknown, label: string): string {
  if (typeof value !== "string" || !FULL_BUILD_COMMIT.test(value)) {
    throw new Error(`${label} must be a full lowercase Git commit SHA`);
  }
  return value;
}
