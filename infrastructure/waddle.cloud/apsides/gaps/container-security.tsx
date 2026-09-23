// GAP-CONTAINER-SECURITY: even the restrictive subset the compiler accepts
// (runAsNonRoot, allowPrivilegeEscalation: false) is rejected by the SDK.
// Rejected with: "Deployment containers[0] contains unsupported field securityContext".
import { Probe, ProbeDeployment } from "./_base.tsx";

export default (
  <Probe>
    <ProbeDeployment container={{ securityContext: { runAsNonRoot: true, allowPrivilegeEscalation: false } }} />
  </Probe>
);
