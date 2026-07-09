import {
  requireExactKeys,
  requireRecord,
} from "../gate-evidence/common";
import {
  RELEASE_PUBLICATION_ISSUER,
  RELEASE_PUBLICATION_REPOSITORY,
  RELEASE_PUBLICATION_SOURCE_REF,
  type PublicationAttestation,
} from "./model";
import {
  exactLiteral,
  requireNonPlaceholderSha256,
} from "./validation";

export function parsePublicationAttestation(
  value: unknown,
  workflow: string,
  workflowCommit: string,
  expectedArtifactSetSha256: string,
  label: string,
): PublicationAttestation {
  const attestation = requireRecord(value, label);
  requireExactKeys(
    attestation,
    [
      "kind",
      "repository",
      "workflow",
      "issuer",
      "sourceRef",
      "workflowCommit",
      "artifactSetSha256",
      "subjectSha256",
      "bundleSha256",
    ],
    label,
  );
  exactLiteral(attestation.kind, "github-sigstore", `${label}.kind`);
  exactLiteral(
    attestation.repository,
    RELEASE_PUBLICATION_REPOSITORY,
    `${label}.repository`,
  );
  exactLiteral(attestation.workflow, workflow, `${label}.workflow`);
  exactLiteral(
    attestation.issuer,
    RELEASE_PUBLICATION_ISSUER,
    `${label}.issuer`,
  );
  exactLiteral(
    attestation.sourceRef,
    RELEASE_PUBLICATION_SOURCE_REF,
    `${label}.sourceRef`,
  );
  exactLiteral(
    attestation.workflowCommit,
    workflowCommit,
    `${label}.workflowCommit`,
  );
  exactLiteral(
    attestation.artifactSetSha256,
    expectedArtifactSetSha256,
    `${label}.artifactSetSha256`,
  );
  return {
    kind: "github-sigstore",
    repository: RELEASE_PUBLICATION_REPOSITORY,
    workflow,
    issuer: RELEASE_PUBLICATION_ISSUER,
    sourceRef: RELEASE_PUBLICATION_SOURCE_REF,
    workflowCommit,
    artifactSetSha256: expectedArtifactSetSha256,
    subjectSha256: requireNonPlaceholderSha256(
      attestation.subjectSha256,
      `${label}.subjectSha256`,
    ),
    bundleSha256: requireNonPlaceholderSha256(
      attestation.bundleSha256,
      `${label}.bundleSha256`,
    ),
  };
}
