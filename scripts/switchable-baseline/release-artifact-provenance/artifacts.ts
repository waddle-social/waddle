import {
  requireExactKeys,
  requireRecord,
  requireString,
} from "../gate-evidence/common";
import {
  GITOPS_REPOSITORY,
  HELM_CHART_REPOSITORY,
  RELEASE_EXTENSION_NAMES,
  SERVER_IMAGE_REPOSITORY,
  type ExtensionArtifact,
  type ObservedArtifactDigests,
  type OciArtifact,
  type ReleaseArtifacts,
  type ReleaseExtensionName,
} from "./model";
import {
  exactLiteral,
  requireNonPlaceholderSha256,
  requireOciDigest,
} from "./validation";

function parseOciArtifact(
  value: unknown,
  repository: string,
  label: string,
): OciArtifact {
  const artifact = requireRecord(value, label);
  requireExactKeys(artifact, ["repository", "digest"], label);
  exactLiteral(artifact.repository, repository, `${label}.repository`);
  return {
    repository,
    digest: requireOciDigest(artifact.digest, `${label}.digest`),
  };
}

function extensionRepository(name: ReleaseExtensionName): string {
  return `${SERVER_IMAGE_REPOSITORY}/extensions/${name}`;
}

function parseExtensionArtifacts(
  value: unknown,
  label: string,
): ExtensionArtifact[] {
  if (!Array.isArray(value)) throw new Error(`${label} must be an array`);
  const extensions = value.map((entry, index) => {
    const extensionLabel = `${label}[${index}]`;
    const extension = requireRecord(entry, extensionLabel);
    requireExactKeys(
      extension,
      ["name", "repository", "digest"],
      extensionLabel,
    );
    const name = requireString(extension, "name", extensionLabel);
    if (!RELEASE_EXTENSION_NAMES.includes(name as ReleaseExtensionName)) {
      throw new Error(`${extensionLabel}.name is not a release extension`);
    }
    const typedName = name as ReleaseExtensionName;
    exactLiteral(
      extension.repository,
      extensionRepository(typedName),
      `${extensionLabel}.repository`,
    );
    return {
      name: typedName,
      repository: extensionRepository(typedName),
      digest: requireOciDigest(extension.digest, `${extensionLabel}.digest`),
    };
  });
  if (
    JSON.stringify(extensions.map(({ name }) => name))
    !== JSON.stringify(RELEASE_EXTENSION_NAMES)
  ) {
    throw new Error(`${label} must use the exact canonical release extension order`);
  }
  return extensions;
}

function parseObservedExtensionDigests(
  value: unknown,
  artifacts: ExtensionArtifact[],
  label: string,
): Array<{ name: ReleaseExtensionName; digest: string }> {
  if (!Array.isArray(value)) throw new Error(`${label} must be an array`);
  const extensions = value.map((entry, index) => {
    const extensionLabel = `${label}[${index}]`;
    const extension = requireRecord(entry, extensionLabel);
    requireExactKeys(extension, ["name", "digest"], extensionLabel);
    const name = requireString(extension, "name", extensionLabel);
    if (!RELEASE_EXTENSION_NAMES.includes(name as ReleaseExtensionName)) {
      throw new Error(`${extensionLabel}.name is not a release extension`);
    }
    return {
      name: name as ReleaseExtensionName,
      digest: requireOciDigest(extension.digest, `${extensionLabel}.digest`),
    };
  });
  if (
    JSON.stringify(extensions.map(({ name }) => name))
    !== JSON.stringify(RELEASE_EXTENSION_NAMES)
  ) {
    throw new Error(`${label} must use the exact canonical release extension order`);
  }
  for (const [index, extension] of extensions.entries()) {
    if (extension.digest !== artifacts[index]?.digest) {
      throw new Error(
        `${label}[${index}].digest does not match the published release artifact`,
      );
    }
  }
  return extensions;
}

export function parseArtifacts(value: unknown): ReleaseArtifacts {
  const label = "release artifact provenance.artifacts";
  const artifacts = requireRecord(value, label);
  requireExactKeys(
    artifacts,
    ["serverImage", "helmChart", "gitOps", "extensions", "web"],
    label,
  );
  const web = requireRecord(artifacts.web, `${label}.web`);
  requireExactKeys(
    web,
    ["artifactSha256", "deploymentIdentitySha256"],
    `${label}.web`,
  );
  return {
    serverImage: parseOciArtifact(
      artifacts.serverImage,
      SERVER_IMAGE_REPOSITORY,
      `${label}.serverImage`,
    ),
    helmChart: parseOciArtifact(
      artifacts.helmChart,
      HELM_CHART_REPOSITORY,
      `${label}.helmChart`,
    ),
    gitOps: parseOciArtifact(
      artifacts.gitOps,
      GITOPS_REPOSITORY,
      `${label}.gitOps`,
    ),
    extensions: parseExtensionArtifacts(
      artifacts.extensions,
      `${label}.extensions`,
    ),
    web: {
      artifactSha256: requireNonPlaceholderSha256(
        web.artifactSha256,
        `${label}.web.artifactSha256`,
      ),
      deploymentIdentitySha256: requireNonPlaceholderSha256(
        web.deploymentIdentitySha256,
        `${label}.web.deploymentIdentitySha256`,
      ),
    },
  };
}

export function parseObservedArtifactDigests(
  value: unknown,
  artifacts: ReleaseArtifacts,
): ObservedArtifactDigests {
  const label = "release artifact provenance.observedDeployment.artifactDigests";
  const digests = requireRecord(value, label);
  requireExactKeys(
    digests,
    ["serverImageDigest", "helmChartDigest", "gitOpsDigest", "extensions"],
    label,
  );
  const result = {
    serverImageDigest: requireOciDigest(
      digests.serverImageDigest,
      `${label}.serverImageDigest`,
    ),
    helmChartDigest: requireOciDigest(
      digests.helmChartDigest,
      `${label}.helmChartDigest`,
    ),
    gitOpsDigest: requireOciDigest(
      digests.gitOpsDigest,
      `${label}.gitOpsDigest`,
    ),
    extensions: parseObservedExtensionDigests(
      digests.extensions,
      artifacts.extensions,
      `${label}.extensions`,
    ),
  };
  if (
    result.serverImageDigest !== artifacts.serverImage.digest
    || result.helmChartDigest !== artifacts.helmChart.digest
    || result.gitOpsDigest !== artifacts.gitOps.digest
  ) {
    throw new Error(`${label} does not match the published release artifacts`);
  }
  return result;
}
