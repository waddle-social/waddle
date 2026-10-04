// GAP-VOLUMES: the chart mounts the OpenRouter key file the ai-chatbot
// extension reads (server/deployment.cue), a data emptyDir and the XMPP TLS
// certificate (unused by the server today).
// Rejected with: "Deployment.spec.template.spec contains unsupported field volumes".
import { Probe, ProbeDeployment } from "./_base.tsx";

export default (
  <Probe>
    <ProbeDeployment
      pod={{ volumes: [{ name: "xmpp-tls", secret: { secretName: "waddle-xmpp-tls" } }] }}
      container={{ volumeMounts: [{ name: "xmpp-tls", mountPath: "/etc/waddle/tls", readOnly: true }] }}
    />
  </Probe>
);
