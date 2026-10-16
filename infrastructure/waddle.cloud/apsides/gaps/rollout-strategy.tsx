// GAP-ROLLOUT-STRATEGY: maxSurge 1 / maxUnavailable 0, and the one-shot
// Recreate cutovers the HelmRelease comments record.
// Rejected with: "Deployment.spec contains unsupported field strategy".
import { Probe, ProbeDeployment } from "./_base.tsx";

export default (
  <Probe>
    <ProbeDeployment spec={{ strategy: { type: "RollingUpdate", rollingUpdate: { maxSurge: 1, maxUnavailable: 0 } } }} />
  </Probe>
);
