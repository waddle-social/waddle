// GAP-DOWNWARD-API: WADDLE_CLUSTERING_POD_TEMPLATE_HASH reads the Pod's
// pod-template-hash label; only literal values and secretKeyRef are allowed.
// Rejected with: "Deployment containers[0].env[0].valueFrom contains unsupported field fieldRef".
import { Probe, ProbeDeployment } from "./_base.tsx";

export default (
  <Probe>
    <ProbeDeployment
      container={{
        env: [{
          name: "WADDLE_CLUSTERING_POD_TEMPLATE_HASH",
          valueFrom: { fieldRef: { fieldPath: "metadata.labels['pod-template-hash']" } },
        }],
      }}
    />
  </Probe>
);
