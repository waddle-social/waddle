import type { ReleaseArtifactProvenanceVerifier } from "./model";

export const verifyTrustedReleaseArtifactProvenance: ReleaseArtifactProvenanceVerifier =
  async () => {
    throw new Error(
      "release-artifact-provenance blocker: trusted publication and deployed-artifact attestation verification is not implemented",
    );
  };
