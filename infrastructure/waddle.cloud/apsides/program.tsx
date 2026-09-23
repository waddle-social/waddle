import { Controller } from "@apsides/kubernetes";
import { Ingress } from "./ingress.tsx";
import { WaddleDatabase } from "./postgres.tsx";
import { RuntimeSecrets } from "./runtime-secrets.tsx";
import { namespace } from "./site.ts";
import { SpiceDB } from "./spicedb.tsx";
import { WaddleServer } from "./waddle-server.tsx";

// One Apsides domain per workload namespace: this program owns what
// ../gitops/kustomization-infra-waddle-server.yaml applies to `waddle`.
export default (
  <Controller
    id="waddle"
    namespace={namespace}
    controlNamespace="apsides-system"
    clusterBinding="waddle-production"
    policyRef="platform/waddle"
    enforcement="api-bound"
    budgets={{
      maxResources: 32,
      maxInputs: 16,
      maxReactions: 8,
      maxObjectBytes: 262_144,
      maxQueueDepth: 64,
      maxConcurrentRequests: 4,
    }}
  >
    <WaddleDatabase />
    <SpiceDB />
    <RuntimeSecrets />
    <Ingress />
    <WaddleServer />
  </Controller>
);
