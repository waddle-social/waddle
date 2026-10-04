import { Certificate } from "./.aps/crds/certificate.ts";
import { HTTPRoute } from "./.aps/crds/httproute.ts";
import { ReferenceGrant } from "./.aps/crds/referencegrant.ts";
import { gateway, namespace, xmppHost } from "./site.ts";

export const Ingress = () => (
  <>
    <Certificate
      metadata={{ name: "waddle-xmpp-tls", namespace }}
      spec={{
        secretName: "waddle-xmpp-tls",
        issuerRef: { name: "letsencrypt-production", kind: "ClusterIssuer" },
        dnsNames: [xmppHost],
      }}
    />
    <HTTPRoute
      metadata={{ name: "waddle-server", namespace }}
      spec={{
        parentRefs: [{ ...gateway, sectionName: "https-xmpp" }],
        hostnames: [xmppHost],
        rules: [{ backendRefs: [{ name: "waddle-server", port: 3000 }] }],
      }}
    />
    <HTTPRoute
      metadata={{ name: "waddle-server-http-redirect", namespace }}
      spec={{
        parentRefs: [{ ...gateway, sectionName: "http-xmpp" }],
        hostnames: [xmppHost],
        rules: [{ filters: [{ type: "RequestRedirect", requestRedirect: { scheme: "https", statusCode: 301 } }] }],
      }}
    />
    <ReferenceGrant
      metadata={{ name: "gateway-to-waddle", namespace }}
      spec={{
        from: [{ group: "gateway.networking.k8s.io", kind: "HTTPRoute", namespace }],
        to: [{ group: "", kind: "Service" }],
      }}
    />
  </>
);
