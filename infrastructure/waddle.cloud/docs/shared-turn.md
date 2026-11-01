# Shared TURN relay

The coturn service is independent of the SFU. Waddle's XEP-0215 credentials
and LiveKit's signal-provided ICE servers use the same TURN REST secret from
the `livekit-sfu` secret item's `turn-shared-secret` property. Neither the
secret nor generated credentials belong in Helm values or ConfigMaps.

`livekit-turn-config` is an ExternalSecret with a raw coturn secret and a
quoted `rtc.turn_servers` fragment. The SFU composes that fragment with its
base configuration in a mode-0600 pod-local file. Embedded TURN stays off.
LiveKit v1.11 supports the external server `secret` and `ttl` fields, so this
change does not require the later SFU upgrade.

## Traffic and trust boundary

- Clients reach `turn.waddle.social:30478/UDP` through the existing
  Helm-owned NodePort, whose selector now targets coturn. TLS on port 443
  passes through the Gateway to coturn and terminates with the existing
  cert-manager certificate.
- coturn advertises its **pod IP** for UDP relay allocations in
  `49152–49251`. The SFU can reach that address directly. The client sends
  data over its TURN control connection; no public allocation port range
  is needed. Do not add external-IP translation to coturn without changing
  and requalifying this routing design.
- This deployment has one Kubernetes node. The downward API `status.hostIP`
  must equal the SFU's advertised public IPv4. That equality was observed
  during the 2026-11-01 planning/implementation investigation; it is a
  preflight gate, not a permanent platform assumption.
- coturn denies all peer addresses except that exact node IPv4. Cilium
  independently limits relay egress to the SFU's UDP media port, 30882.
  The SFU policy explicitly allows both directions to private coturn
  allocations. IPv6 peers and TCP relay endpoints are disabled.
- TURN authenticates relay use; it does not authorize XMPP membership or
  decrypt DTLS-SRTP media. The SFU remains inside the media trust boundary.

Multi-node work must expand the exact SFU peer set and destination policy
together. An address allowlist alone does not constrain destination ports.
NodePort/cloud firewall access remains separate from pod policy; keep
deployment-specific perimeter evidence in the private advisory.

## Qualification and rollout

Use a maintenance window for the first transition: the current single SFU
restarts when its configuration changes. First ensure the ESO credential
Secret and TLS certificate are ready and the coturn Deployment passes its
TLS readiness probe. Publish chart 0.1.9 before reconciling its HelmRelease.

1. Compare the node `InternalIP`, DNS A for `sfu.waddle.social`, and actual
   SFU ICE candidates. The coturn peer list must match the candidate address.
   Stop if they disagree or the media path uses unqualified IPv6 candidates.
2. Inspect the rendered configuration and mounted Secret references. Never
   print the raw configuration or shared secret into logs or PR evidence.
3. Prove authenticated Allocate and bidirectional UDP relay in an isolated
   environment, through both TURN/UDP and TURN/TLS with certificate
   validation. A STUN Binding reply alone is **not** relay acceptance. The
   verifier requires an explicit UDP echo peer; production SFU media is not
   an echo service and must be tested with real WebRTC clients instead.
4. With the production Cilium policy, prove the coturn-to-public-NodePort
   hairpin reaches the SFU and the SFU reaches coturn's private allocation
   IP/range. Capture flows as evidence. Endpoint selection after DNAT is
   platform behavior that local Docker media tests do not establish.
5. From a restricted external network, force relay on web and Android and
   prove two-way media over UDP and TLS:443. Record a selected local ICE
   candidate of type `relay`, media counters in both directions, reconnect
   behavior and wrong-secret rejection. Test unrelated IPv4/IPv6 peers and
   a disallowed port on the allowed SFU address as negative cases.
6. Roll coturn with allocations active. `preStop` sends SIGUSR1 to the
   foreground turnserver PID and **waits** for it to exit; returning from
   the hook immediately would let Kubernetes send SIGTERM too early.
   Existing allocations must continue while new allocations use the new
   ready pod. Grace is four hours; the Flux Kustomization timeout exceeds
   that grace. Calls beyond the limit are not promised uninterrupted relay.

The TLS readiness probe validates the served certificate's chain and
`turn.waddle.social` name against the image's CA store. It connects to the
bound pod address rather than loopback. A bad/expired certificate must keep
the pod unready. Projected Secret changes do not by themselves prove that
the running process reloaded them; until #1554, certificate/secret rotation
uses a controlled drain and restart. Overlap credentials when rotating.

## Rollback

Before changing the NodePort selector or TLSRoute, stop admitting new
allocations to the retiring coturn process and let its active allocations
drain. A revert restores the previous chart/route topology, including its
known credential mismatch; it is not proof that restricted-network calls
work. Retain the credential and certificate Secrets until their readers and
allocations are gone. Prefer roll-forward for a partial transition, and
record the SFU restart/maintenance impact explicitly.

## Sources

- [LiveKit v1.11 external TURN config](https://github.com/livekit/livekit/blob/v1.11.0/pkg/config/config.go)
- [coturn 4.18.0 peer ACL, relay and drain options](https://github.com/coturn/coturn/blob/4.18.0/README.turnserver)
- [coturn 4.18.0 SIGUSR1 drain handler](https://github.com/coturn/coturn/blob/4.18.0/src/apps/relay/mainrelay.c)
- [Official coturn image release](https://github.com/coturn/coturn/releases/tag/docker/4.18.0-r0)

The pinned `coturn/coturn:4.18.0` digest is the multi-platform index verified
from the official registry on 2026-11-01, including linux/amd64 and arm64.

## Final local qualification

Two browser participants using livekit-client 2.21.0 and headless Chromium 156 exchanged camera and microphone media through the pinned coturn image and LiveKit v1.11.0. Both forced-relay UDP and TLS runs selected relay candidates, decoded video on each participant, and showed increasing media byte/frame counters. A further TLS run omitted client ICE servers and successfully used credentials generated by LiveKit from rtc.turn_servers. The browser used an explicit SPKI trust exception for the local fixture certificate; independent Go TLS tests enforce CA/hostname validation and reject untrusted certificates. This isolated private-IP Docker setup does not prove the production Cilium/public-NodePort, Android or external-network gates.

The exact rendered SFU command also starts the pinned v1.11.0 image with read-only inputs and root filesystem, preserving quoted secret content in a mode-0600 runtime file; missing fragments fail before startup. See [shared-turn-local-evidence.json](shared-turn-local-evidence.json) for the captured evidence.

Three exact-line Sonar exceptions were independently reviewed: the pinned image requires NET_BIND_SERVICE even for execution under its file capability, and the selected classic coturn authentication mode uses the legacy MD5 long-term key and HMAC-SHA1 REST formula. Modern STUN can negotiate SHA-256; these exceptions are confined to this compatibility mode and do not authorize general weak-hash use. No file-wide or rule-wide ignore was added.
