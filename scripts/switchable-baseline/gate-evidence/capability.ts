import {
  fail,
  type CapabilityArtifactManifest,
  type CapabilityArtifactRole,
  type EvidenceDeploymentScope,
} from "./common";
import { validateDiscoTargetContractArtifact } from "./capability-contract";
import { validateLiveDiscoArtifact } from "./capability-live";
import { validateReconciliationArtifact } from "./capability-reconciliation";

/** Validate the complete capability evidence set as one coherent package. */

export function validateCapabilityContents(
  repositoryRoot: string,
  manifest: CapabilityArtifactManifest,
  contents: Map<CapabilityArtifactRole, unknown>,
): EvidenceDeploymentScope {
  const contractArtifact = manifest.artifacts.find(
    ({ role }) => role === "disco-target-contract",
  );
  const liveArtifact = manifest.artifacts.find(
    ({ role }) => role === "live-disco-export",
  );
  const reconciliationArtifact = manifest.artifacts.find(
    ({ role }) => role === "capability-reconciliation",
  );
  if (!contractArtifact || !liveArtifact || !reconciliationArtifact) {
    fail("capability artifact roles are incomplete");
  }
  const contract = validateDiscoTargetContractArtifact(
    repositoryRoot,
    contents.get("disco-target-contract"),
    contractArtifact,
  );
  const live = validateLiveDiscoArtifact(
    contents.get("live-disco-export"),
    liveArtifact,
    contract,
  );
  const reconciliation = validateReconciliationArtifact(
    repositoryRoot,
    contents.get("capability-reconciliation"),
    reconciliationArtifact,
    liveArtifact,
    live,
    contract,
  );
  if (Date.parse(manifest.capturedAt) < Date.parse(reconciliation.capturedAt)) {
    fail("artifact-manifest.capturedAt must not precede capability reconciliation");
  }
  return reconciliation.scope;
}
