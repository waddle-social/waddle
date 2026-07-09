import {
  parseRelease,
  releasesEqual,
  requireExactKeys,
  requireRecord,
  type EvidenceRelease,
} from "../gate-evidence/common";
import type { ReplicaProvenance } from "../replica-provenance";
import { parseArtifacts } from "./artifacts";
import { parseObservedDeployment } from "./deployment";
import {
  serverReleaseArtifactSetSha256,
  webReleaseArtifactSetSha256,
} from "./digest";
import {
  SERVER_PUBLICATION_WORKFLOW,
  WEB_DEPLOYMENT_PROJECT,
  WEB_DEPLOYMENT_PROVIDER,
  WEB_PUBLICATION_WORKFLOW,
  type ReleaseArtifactProvenance,
} from "./model";
import { parsePublicationAttestation } from "./publication";
import {
  exactLiteral,
  requireNonPlaceholderSha256,
} from "./validation";

export function parseReleaseArtifactProvenance(
  value: unknown,
  release: EvidenceRelease,
  replicaProvenance: ReplicaProvenance,
): ReleaseArtifactProvenance {
  const label = "release artifact provenance";
  const provenance = requireRecord(value, label);
  requireExactKeys(
    provenance,
    [
      "schemaVersion",
      "release",
      "artifacts",
      "observedDeployment",
      "observedWeb",
      "publicationAttestations",
    ],
    label,
  );
  if (provenance.schemaVersion !== 1) {
    throw new Error(`${label}.schemaVersion must be 1`);
  }
  const parsedRelease = parseRelease(provenance.release, `${label}.release`);
  if (!releasesEqual(parsedRelease, release)) {
    throw new Error(`${label}.release does not match the live collection release`);
  }
  const artifacts = parseArtifacts(provenance.artifacts);
  const observedDeployment = parseObservedDeployment(
    provenance.observedDeployment,
    artifacts,
    replicaProvenance,
  );
  const observedWeb = requireRecord(
    provenance.observedWeb,
    `${label}.observedWeb`,
  );
  requireExactKeys(
    observedWeb,
    [
      "provider",
      "project",
      "artifactSha256",
      "deploymentIdentitySha256",
      "webCommit",
    ],
    `${label}.observedWeb`,
  );
  exactLiteral(
    observedWeb.provider,
    WEB_DEPLOYMENT_PROVIDER,
    `${label}.observedWeb.provider`,
  );
  exactLiteral(
    observedWeb.project,
    WEB_DEPLOYMENT_PROJECT,
    `${label}.observedWeb.project`,
  );
  exactLiteral(
    observedWeb.webCommit,
    release.webCommit,
    `${label}.observedWeb.webCommit`,
  );
  const observedWebArtifactSha256 = requireNonPlaceholderSha256(
    observedWeb.artifactSha256,
    `${label}.observedWeb.artifactSha256`,
  );
  const observedWebIdentitySha256 = requireNonPlaceholderSha256(
    observedWeb.deploymentIdentitySha256,
    `${label}.observedWeb.deploymentIdentitySha256`,
  );
  if (
    observedWebArtifactSha256 !== artifacts.web.artifactSha256
    || observedWebIdentitySha256 !== artifacts.web.deploymentIdentitySha256
  ) {
    throw new Error(`${label}.observedWeb does not match the published web artifact`);
  }
  const publicationAttestations = requireRecord(
    provenance.publicationAttestations,
    `${label}.publicationAttestations`,
  );
  requireExactKeys(
    publicationAttestations,
    ["server", "web"],
    `${label}.publicationAttestations`,
  );
  const serverSetSha256 = serverReleaseArtifactSetSha256(
    parsedRelease,
    artifacts,
  );
  const webSetSha256 = webReleaseArtifactSetSha256(parsedRelease, artifacts);
  return {
    schemaVersion: 1,
    release: parsedRelease,
    artifacts,
    observedDeployment,
    observedWeb: {
      provider: WEB_DEPLOYMENT_PROVIDER,
      project: WEB_DEPLOYMENT_PROJECT,
      artifactSha256: observedWebArtifactSha256,
      deploymentIdentitySha256: observedWebIdentitySha256,
      webCommit: release.webCommit,
    },
    publicationAttestations: {
      server: parsePublicationAttestation(
        publicationAttestations.server,
        SERVER_PUBLICATION_WORKFLOW,
        release.serverCommit,
        serverSetSha256,
        `${label}.publicationAttestations.server`,
      ),
      web: parsePublicationAttestation(
        publicationAttestations.web,
        WEB_PUBLICATION_WORKFLOW,
        release.webCommit,
        webSetSha256,
        `${label}.publicationAttestations.web`,
      ),
    },
  };
}
