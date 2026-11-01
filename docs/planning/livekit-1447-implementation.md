# Issue #1447: Align shared TURN relay with issued credentials

Source: https://github.com/waddle-social/waddle/issues/1447

## Implementation plan

1. Independently review shared coturn topology, peer restrictions, pod relay reachability, credentials and drain behavior before implementation.
2. Deploy digest-pinned coturn through GitOps, sharing the existing TURN secret; configure LiveKit rtc.turn_servers and disable embedded TURN.
3. Replace binding-only verification with authenticated Allocate and bidirectional relay checks over UDP and TLS.
4. Verify rendered configuration, restricted relay/drain behavior and available real-media clients; document rollout, rollback and remaining device/network gates.

## Delivery

Use this isolated issue branch and PR. Preserve unrelated workspace changes. Keep application semantics XMPP-native, reuse existing dependencies, and record native fixture results separately from live provider or production acceptance. Do not merge or deploy as part of preparing the implementation.

## Implementation evidence

- Production-posture regression failed before the change because embedded TURN was enabled, then passed with the shared credential Secret selected.
- Focused TURN/chart regressions pass, covering the retained NodePort selector, mutual exclusion of TURN modes, private runtime configuration, credential source/TTL and relay restrictions. Selected-file Go vet, production-equivalent Helm lint, and Kustomize build pass.
- Official coturn 4.18.0 multi-platform image digest was verified from Docker Registry and exercised on local arm64. The actual ConfigMap startup/readiness scripts start coturn and validate its bound TLS listener with a qualification CA; substituting an untrusted CA makes readiness fail, and restoring trust makes it pass.
- Authenticated UDP and TLS Allocate/permission/channel/bidirectional echo pass against that actual GitOps configuration. Wrong credentials, an unallowed IPv4 peer and an untrusted certificate are rejected. The self-cleaning verifier qualification fixture is under internal/turnprobe/testdata.
- Native drain experiment sent SIGUSR1 during a real allocation: all 12 payloads were echoed with zero loss while preStop stayed blocked. A local-only shortened allocation lifetime then proved clean turnserver exit (code 0) after expiry. The exec hook is terminated with the PID namespace when the foreground PID1 exits; its exec status is not the server's exit status. Production lifetime remains 3600 seconds and grace four hours.
- Main's unrelated Go test failures are tracked by #2141 (undefined Gateway API fixture and runtime-secret mapping count). Direct CUE vet reproduces the same cuenv schema failures on untouched main; these changes do not repair that separate baseline.

## Remaining operational acceptance

This branch does not deploy. Production Cilium public-NodePort hairpin/private-allocation flow, restricted-network UDP and TLS443 WebRTC on web and Android, first-transition maintenance, production certificate trust, and live drain/rollout remain explicit gates in infrastructure/waddle.cloud/docs/shared-turn.md. Local packet and browser qualification do not satisfy those gates.

