// GAP-CROSS-NAMESPACE: a controller manages exactly one namespace, so the
// Gateway certificates in `default` (../../gitops/cilium-gateway) need their
// own domain, release and controller generation.
// Rejected with: "Certificate xmpp-waddle-social-tls is outside domain namespace waddle; cross-domain operations are unsupported".
import { defineNamespacedResource } from "@apsides/kubernetes";
import { Probe } from "./_base.tsx";

const Certificate = defineNamespacedResource<Record<string, unknown>>({
  apiVersion: "cert-manager.io/v1",
  kind: "Certificate",
  apiResource: { plural: "certificates", scope: "Namespaced", schemaSha256: `sha256:${"0".repeat(64)}` },
});

export default (
  <Probe>
    <Certificate metadata={{ name: "xmpp-waddle-social-tls", namespace: "default" }} spec={{ secretName: "xmpp-waddle-social-tls" }} />
  </Probe>
);
