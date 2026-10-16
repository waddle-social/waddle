// GAP-HEADLESS-SERVICE: waddle-server-headless (clusterIP None,
// publishNotReadyAddresses) is the clustering bootstrap peer list.
// Rejected with: "Service.spec contains unsupported field clusterIP".
import { Service } from "@apsides/kubernetes";
import { labels, namespace, Probe } from "./_base.tsx";

const spec = {
  clusterIP: "None",
  publishNotReadyAddresses: true,
  selector: labels,
  ports: [{ name: "swarm", port: 7900, targetPort: "swarm" }],
};

export default (
  <Probe>
    <Service metadata={{ name: "waddle-server-headless", namespace }} spec={spec as { selector: typeof labels; ports: typeof spec.ports }} />
  </Probe>
);
