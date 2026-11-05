# LiveKit webhook delivery

LiveKit sends a raw ProtoJSON body signed by the dedicated webhook key. The
SFU delivers it directly to
`http://waddle-server.waddle.svc.cluster.local:3000/api/v1/livekit/webhook`.
The SFU's Cilium policy permits only the server endpoints in the waddle
namespace on TCP/3000. The public HTTPS handler still verifies signatures for
external requests; the SFU no longer depends on a public gateway hairpin.

This Service path is plaintext HTTP within the existing cluster trust boundary.
The JWT signature and body hash authenticate payloads; they do not encrypt
them. This change does not establish transport TLS or replace the server's
signature/replay checks.

## Reproduce delivery safely

The verifier needs Python with the existing PyYAML tooling, an explicit
Kubernetes context from the local kubectl inventory, read access to the two existing webhook secrets, and exec
access to a ready SFU pod containing BusyBox wget. It resolves the configured
webhook-key inside the **SFU** keys.yaml and checks that the server's
verifier secret matches, without printing either. Malformed credential YAML or
value shapes produce a generic error without parser input or traceback. The
JWT travels to kubectl through stdin and only HTTP statuses are printed. It
rejects option-like, non-printable and unknown contexts before cluster access,
and never creates or deletes policies.

After the GitOps policy is applied:

```sh
python3 infrastructure/waddle.cloud/scripts/verify-livekit-webhook-delivery.py \
  --context <explicit-context>
```

Expected results: signed internal POST = 200; invalid signature = 401. The
probe uses an unknown event with no room, participant or event ID, so it cannot
tear down a call or create a persistent delivery row. It does increment
`event="other",outcome="received"` and emit an acceptance log: record the probe
time, because that synthetic increment can suppress the silence alert for its
six-hour lookback. A probe is not evidence that genuine lifecycle events flowed.

For a pre-rollout comparison, an operator can add `--compare-public`. If the
internal path is still denied, the temporary diagnostic allowance used for the
investigation was exactly this additive policy:

```yaml
apiVersion: cilium.io/v2
kind: CiliumNetworkPolicy
metadata:
  generateName: webhook-delivery-probe-
  namespace: livekit
spec:
  endpointSelector:
    matchLabels:
      app.kubernetes.io/name: livekit-sfu
      app.kubernetes.io/instance: livekit-sfu
  egress:
    - toEndpoints:
        - matchLabels:
            k8s:io.kubernetes.pod.namespace: waddle
            k8s:app.kubernetes.io/name: waddle-server
            k8s:app.kubernetes.io/instance: waddle-server
      toPorts:
        - ports:
            - port: "3000"
              protocol: TCP
```

Save it as `/tmp/webhook-probe-policy.yaml`. An operator can create it, record
the generated name, run the verifier, and delete that exact policy afterward:

```sh
kubectl --context=<explicit-context> create -f /tmp/webhook-probe-policy.yaml
python3 infrastructure/waddle.cloud/scripts/verify-livekit-webhook-delivery.py \
  --context=<explicit-context> --compare-public
kubectl --context=<explicit-context> delete ciliumnetworkpolicy -n livekit <generated-name> --wait=true
kubectl --context=<explicit-context> get ciliumnetworkpolicy -n livekit
```

Do not modify the managed policy or restart workloads for this comparison. The
temporary allowance and its cleanup are explicit operator actions, separate
from the retained verifier. Do not run them from CI.

Use Hubble to inspect the SFU source and server TCP/3000 path around the probe
time. Export only timestamp, verdict, source/destination, port, TCP flags and
drop reason. Raw L7 flow data can contain the Authorization header and must not
be checked in or pasted into a PR.

## Evidence collected 2026-11-01

- A signed, effect-free POST originating in the existing SFU pod returned 403
  through the configured public HTTPS URL. The workstation's GET returned 405,
  so external reachability did not establish SFU delivery.
- Hubble showed the public TCP handshake forwarded, followed by an HTTP L7
  drop from reserved ingress to reserved host. No credential change was needed
  to diagnose this path failure.
- The internal Service initially had a policy-denied SYN. With the temporary
  endpoint-scoped allow, the signed POST returned 200 and an invalid signature
  returned 401. Hubble then showed forwarded SYN/ACK/PSH traffic to server
  TCP/3000.
- The server recorded the accepted `other` event at
  `2026-11-01T22:36:00.019731Z` and the invalid-token rejection at
  `2026-11-01T22:36:12.121349Z`. The temporary policy was removed afterward.
- Before its optional policy mutation was removed, the verifier itself was
  executed with an explicit context, public comparison and temporary egress:
  it reported 403/200/401 and confirmed policy cleanup. The retained helper now
  only reads credentials and sends probes; temporary policy management remains
  the explicit operator procedure above.

These observations prove the diagnosed routing boundary and signed handler
reachability. They do not claim the committed GitOps change has deployed or
that a genuine production call's complete event lifecycle has been observed.

## Payload and alert regressions

Upstream WebhookEvent declares created_at as int64; its ProtoJSON encoding is a
decimal string. Four fixtures generated with LiveKit v1.11.0's actual protocol
dependency cover participant join/leave/abort and room finish. The signed parser
tests preserve integer precision, accept missing/null/numeric historical forms,
and reject malformed or overflowing timestamps.

```sh
cd server
cargo test -p waddle-sfu --locked webhook::tests
cargo clippy -p waddle-sfu --all-targets --locked -- -D warnings
```

The HTTP call integration helpers also send canonical string timestamps and
`application/webhook+json`, keeping their existing Muji cleanup and call-thread
assertions on the real signed HTTP route.

The existing LiveKitWebhookSilent alert is unchanged. Dedicated promtool cases
prove that active calls with zero or missing accepted-event telemetry alert
after the one-hour hold, idle traffic stays quiet, accepted events from either
replica keep the path healthy, and delivery recovery clears the alert:

```sh
cd infrastructure/waddle.cloud
sh rules/tests/run.sh
```

Credential-error output is tested at the actual verifier CLI boundary with a
synthetic kubectl executable; the tests never contact a cluster:

```sh
python3 -m unittest discover -s infrastructure/waddle.cloud/scripts \
  -p test_verify_livekit_webhook_delivery.py
```

## Rollout and rollback

Render the production GitOps manifests and run the manifest contract test
before publishing. Coordinate the server parser image with the existing server
cutover gate; that gate is separate from the SFU configuration.

Apply the internal egress policy before the SFU restarts with the new URL. An
SFU restart can interrupt calls under the current single-pod topology, so use
the existing authorized maintenance process. Afterward, run the signed probe,
then place a real call and confirm typed participant/room events and cleanup.

A configuration rollback restores the previous URL/policy, but also restores
the known public-hairpin delivery failure. Prefer retaining the scoped internal
allow while repairing any newly discovered problem. Reverting the parser also
restores rejection of canonical string timestamps. Record real event/alert
acceptance on #1616 before closing it.

## Deployed alert verification

Read-only Grafana Cloud queries confirmed a firing LiveKitWebhookSilent sample
at 2026-10-31T01:29:57.489Z. The six-hour setup increase at that sample was
positive (about 2.037, including Prometheus extrapolation), while the six-hour
accepted-webhook increase was 0. This proves that the deployed rule detects
active-call silence, independently of the promtool scenarios.

To reproduce against datasource grafanacloud-prom, use these instant queries:

```promql
ALERTS{alertname="LiveKitWebhookSilent",alertstate="firing"} @ 1793410197.489
sum(increase(waddle_call_setup_ok_total[6h] @ 1793410197.489))
sum(increase(waddle_call_webhook_events_total{outcome="received"}[6h] @ 1793410197.489)) or vector(0)
```

At the end of this diagnostic pass, the accepted-event counter was 2, matching
the two synthetic probes. The rule currently evaluates healthy/normal; this
is not evidence that the unmerged delivery configuration has been deployed.
