# Issue #1447: Align shared TURN relay with issued credentials

Source: https://github.com/waddle-social/waddle/issues/1447

## Implementation plan

1. Independently review shared coturn topology, peer restrictions, pod relay reachability, credentials and drain behavior before implementation.
2. Deploy digest-pinned coturn through GitOps, sharing the existing TURN secret; configure LiveKit rtc.turn_servers and disable embedded TURN.
3. Replace binding-only verification with authenticated Allocate and bidirectional relay checks over UDP and TLS.
4. Verify rendered configuration, restricted relay/drain behavior and available real-media clients; document rollout, rollback and remaining device/network gates.

## Delivery

Use this isolated issue branch and PR. Preserve unrelated workspace changes. Keep application semantics XMPP-native, reuse existing dependencies, and record native fixture results separately from live provider or production acceptance. Do not merge or deploy as part of preparing the implementation.

