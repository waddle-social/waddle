# LiveKit Upgrade Path Research

**Date:** 2026-07-26
**Scope:** waddle repo — `infrastructure/waddle.cloud/charts/livekit-sfu` (Helm chart, currently `livekit-server` v1.11.0) and `chat/` frontend (`livekit-client`, `@livekit/track-processors`). Ticket-driven primary-source research; all version/date claims verified against live GitHub Releases/API and docs.livekit.io as of 2026-07-26, not training-data recall.

## Summary Table

| Component | Current (waddle) | Latest stable | Release date | Source |
|---|---|---|---|---|
| `livekit/livekit-server` (image) | v1.11.0 (chart `appVersion`, `values.yaml image.tag`) | **v1.13.4** | 2026-07-18 | [livekit/livekit releases](https://github.com/livekit/livekit/releases) |
| Helm chart `livekit-sfu` | 0.1.7 (in-repo chart, not upstream) | n/a (waddle-owned chart) | — | `infrastructure/waddle.cloud/charts/livekit-sfu/Chart.yaml` |
| `livekit-client` (JS SDK) | ^2.19.1 | **v2.21.0** | 2026-07-23 | [livekit/client-sdk-js releases](https://github.com/livekit/client-sdk-js/releases) |
| `@livekit/track-processors` | ^0.7.2 | **v0.7.2** (already latest) | 2026-02-25 | [livekit/track-processors-js releases](https://github.com/livekit/track-processors-js/releases) |
| `livekit/egress` | not deployed in waddle infra (no chart/values found) | v1.13.0 | 2026-05-28 | [livekit/egress releases](https://github.com/livekit/egress/releases) |

Full server release train between our pinned v1.11.0 and latest: v1.11.0 (2026-04-17) → v1.12.0 (2026-05-16) → v1.13.1 (2026-06-08) → v1.13.2 (2026-06-27) → v1.13.3 (2026-07-03) → v1.13.4 (2026-07-18). Note there is no separate "v1.13.0" server tag — v1.13 starts at v1.13.1.

`chat/package.json` (read directly) confirms the only LiveKit-related deps are `livekit-client` `^2.19.1` and `@livekit/track-processors` `^0.7.2` — no `livekit-server-sdk`, `@livekit/components-react`, or similar in this repo.

## Recommended Upgrade Path

1. **Server first, then client — but read the v1.12.0 TURN notice before touching either.** v1.12.0 introduced a breaking change to TURN authentication/permission handling that was *soft-deprecated* in v1.12.0 (backwards-compatible) and then **hard-removed in v1.13.1** (PR [#4539](https://github.com/livekit/livekit/pull/4539)). Since our chart currently ships with `livekit.turn.enabled: false` (see `values.yaml`), this specific break does not currently bite us in production, but it must be evaluated before flipping `turn.enabled: true` on any future server version ≥ v1.13.1.
2. Recommended hop order for the server: v1.11.0 → v1.12.0 → v1.13.4 in one Helm bump (single-replica chart per `validations.yaml`, so this is a single pod replace, not a rolling multi-version soak). No intermediate mandatory hop is documented by upstream; v1.12.0 is the only release with a deliberate "backwards compatible for one more release" grace window, and we're jumping straight past that window to v1.13.4, so config must be updated for the new TURN behavior *before* the upgrade if TURN is ever enabled (see below).
3. Client SDK: bump `livekit-client` from `^2.19.1` to `^2.21.0` in the same change or a closely-following one. No hard breaking API removals were found between 2.19.1 and 2.21.0 (all "Minor Changes" in the changesets are additive: data-stream v2 support, `FrameMetadata` rename from `PacketTrailer`, new `applyConstraints` on `LocalAudioTrack`). The `PacketTrailer` → `FrameMetadata` rename in v2.20.0 is the only nominally "breaking" rename — grep the codebase for `PacketTrailer` usage before bumping past 2.19.x.
4. `@livekit/track-processors` requires no action — waddle is already pinned to the latest published version (v0.7.2, 2026-02-25).
5. Egress is not deployed anywhere in this repo's infra (`grep -ril egress infrastructure/` only found alert rules, an Alloy Helm release, and a NetworkPolicy — no egress chart/deployment). No egress-specific upgrade action needed unless/until an egress deployment is added; if one is added later, pair it with server ≥ v1.13.x (egress v1.13.0 released 2026-05-28, contemporaneous with server v1.13.x).
6. Compat-matrix caveat: LiveKit does not publish a formal client/server compatibility matrix as a table; in practice the JS SDK and server evolve independently and are protocol-versioned (SDK negotiates protocol version with the server at connect time), so there is no strict "must upgrade together" requirement — but do not defer the client bump indefinitely, since data-track/data-stream features (data tracks enabled by default in server v1.11.0; data streams v2 added in client v2.21.0) assume both sides are reasonably current.

## Breaking Changes / Deprecations Affecting Our Helm Chart

Cross-checked every key in `infrastructure/waddle.cloud/charts/livekit-sfu/values.yaml` (`livekit.*`, `nodePorts.*`, `turn.*`) against the v1.12.0–v1.13.4 changelogs:

| Chart key (values.yaml / configmap) | Current value | What changed upstream | Action needed |
|---|---|---|---|
| `livekit.turn.enabled` | `false` | v1.12.0 introduced `allow_restricted_peer_cidrs` / `deny_peer_cidrs` (default-deny for private/loopback/link-local/multicast CIDRs) and a TURN credential `ttl_seconds` (default 300); v1.13.1 **removed** the backwards-compat shim entirely (PR [#4539](https://github.com/livekit/livekit/pull/4539)). None of this fires while TURN is disabled. | No action now. **Before ever setting `turn.enabled: true`** on server ≥ v1.12.0, the chart must add explicit `allow_restricted_peer_cidrs` (if any private/relay traffic needs to reach non-public peers) and be aware TURN credentials now expire (`ttl_seconds`, default 300s) — verify this doesn't break long-lived calls that mint TURN creds once at join. |
| `livekit.turn.relay_range_start` / `relay_range_end` | `30000` / `40000` | Not directly touched by 1.12–1.13 changelogs, but confirmed still a valid `config-sample.yaml` key. | No action. |
| `livekit.rtc.use_external_ip` | `true` | v1.13.1 (#4552/#4554/#4563) added config documentation for two related, previously-undocumented keys: `advertise_internal_ip` and `skip_external_ip_validation`. Neither is set in our values.yaml today. | Evaluate whether `skip_external_ip_validation` should be set — our chart's own `validations.yaml` fails the render if NodePorts are exposed without `use_external_ip: true`, precisely the self-ping/NAT scenario this key addresses. Worth a follow-up spike, not a blocker. |
| `livekit.rtc.tcp_port` / `udp_port` | `30881` / `30882` | No renames or removals found in 1.12–1.13. `port_range_start`/`port_range_end` (alternate to fixed `udp_port`) remain valid alternatives, unchanged. | No action. |
| `livekit.prometheus_port` | `6789` | v1.13.2 added "ability to run pprof on dedicated HTTP server" (#4584) and new join-latency / peer-connection-state Prometheus metrics (#4574/#4616) — additive only, same `prometheus_port` mechanism. | No action; optionally wire up the new pprof port later for debugging. |
| `webhook.*` | `enabled: false` | No changes found to webhook config shape in 1.12–1.13. | No action. |
| `apiKeys.*` / `key_file` | as configured | No changes found. | No action. |
| `livekit.log_level` | `info` | No changes found. | No action. |
| `nodePorts.rtc.*`, `nodePorts.turnUdp.*` | as configured | Unaffected — these are waddle-chart-only NodePort wiring, not upstream LiveKit config keys. | No action. |

No config keys used in our chart were found to be **deprecated or renamed** between v1.11.0 and v1.13.4. The only substantive risk is the TURN auth/permission behavior change (v1.12.0 → hard cutover v1.13.1), which is currently dormant because `turn.enabled: false`.

## TURN / NodePort Specific Notes

- Our chart's `validations.yaml` already hard-fails a render that would leave TURN/RTC NodePorts unreachable (`use_external_ip` guard, NodePort range 30000–32767 checks, TCP/UDP port-match checks) — these guards are chart-authored, not upstream-versioned, and remain valid post-upgrade.
- If TURN is ever turned on: server v1.12.0+ defaults to **denying relay to all restricted-peer CIDRs** (loopback, link-local, multicast, private ranges) unless `allow_restricted_peer_cidrs` is explicitly set. This is a stricter default than v1.11.0's behavior and needs an explicit values.yaml addition — likely irrelevant for public-internet WebRTC peers but relevant if any test/dev traffic transits private ranges.
- TURN credential TTL (default 300s, `ttl_seconds`) is new in v1.12.0 and becomes the only supported behavior from v1.13.1 onward (no more "no-TTL" backwards compat). If waddle's server-side code ever mints TURN credentials directly (outside of standard LiveKit token-based joins), audit for TTL assumptions.
- v1.13.1 also documented (not introduced) `advertise_internal_ip` and `skip_external_ip_validation` config keys — worth adding to `values.yaml`/`configmap.yaml` templating if we hit NAT/self-ping issues post-upgrade, per the existing `validations.yaml` comment about `use_external_ip`.

## Egress Compatibility Notes

- No egress deployment exists in this repo (`infrastructure/waddle.cloud` has no `egress` chart, values, or gitops HelmRelease — only unrelated hits for "egress" in Mimir alert rules, the Grafana Alloy chart, and the livekit-sfu NetworkPolicy).
- If egress is introduced later: `livekit/egress` v1.13.0 (2026-05-28) is the version contemporaneous with server v1.13.x and adds the V2 egress API (unified `StartEgressRequest`/`MediaSource`/`Output`/`StorageConfig`), auto-retry, cgroup-aware memory monitoring/OOM handling, and several reliability fixes (deadlocks, S3 multi-part upload fixes, AV-sync fixes). Pair egress ≥ v1.13.0 with server ≥ v1.13.x if adopted.
- Egress v1.12.0 (2025-12-05) and v1.11.0 (2025-11-17) changelogs contain no breaking API removals relevant to a v1.11.0 → v1.13.4 *server* upgrade — egress is a separate deployable and versioned independently, so this is a non-issue until egress is actually deployed.

## Client API Breaking Changes

**`livekit-client` (^2.19.1 → v2.21.0), from GitHub Releases changesets:**
- v2.19.2 (2026-06-08): patch-only (rtpMap event leak fix, timeout guard fix, FF signalling fix). No API change.
- v2.20.0 (2026-06-24): **`PacketTrailer` renamed to `FrameMetadata`** (#1982) — flagged as a "Minor Change" but is a symbol rename; grep waddle's `chat/` codebase for `PacketTrailer` before bumping past 2.19.x. Also added `restrictOwnAudio` experimental param to `AudioCaptureOptions` (additive), enforced max message size on `publishData` (behavior tightening — large payloads that previously silently worked may now throw/reject).
- v2.20.1 (2026-07-08): patch-only; note published `.d.ts` fix for `NonSharedUint8Array` (previously broke downstream `@livekit/components-*` type-checking — not applicable, we don't depend on components-react).
- v2.20.2 (2026-07-20): patch-only; notable behavior change — reliable data-channel sends now use two-watermark flow control (fill/drain watermarks) instead of unconditional sends, to fix SCTP buffer overflow under concurrent writes (#1995/#2013). This is a robustness fix, not an API break, but worth knowing if waddle has any custom backpressure handling around `publishData`.
- v2.21.0 (2026-07-23): additive — data streams v2 support (#1985), event-buffering-during-resume fix (#2018).
- **Net assessment:** no hard-breaking removals between 2.19.1 and 2.21.0 for waddle's usage; the one rename (`PacketTrailer`→`FrameMetadata`) should be grepped for, and the `publishData` max-message-size enforcement (2.20.0) should be checked against any large-payload send paths.

**`@livekit/track-processors` (^0.7.2 → latest v0.7.2):** already current, no changes to evaluate.

## LiveKit Release Cadence & Support Policy

No formal published LTS/support-window or deprecation policy was found in the `livekit/livekit` README, GitHub repo metadata, or docs.livekit.io self-hosting deployment page — this was checked directly via WebFetch on both and via web search; none returned an explicit versioning/support-window statement (see [livekit/livekit README](https://github.com/livekit/livekit) and [docs.livekit.io self-hosting deployment](https://docs.livekit.io/home/self-hosting/deployment/)).

Empirically, from the release timestamps pulled via the GitHub API ([livekit/livekit releases](https://github.com/livekit/livekit/releases)):
- Patch releases land roughly every 1–3 weeks (e.g. v1.13.1 → v1.13.2 → v1.13.3 → v1.13.4 spanning 2026-06-08 to 2026-07-18, ~6 weeks for 3 patches).
- Minor version bumps (breaking-change carriers, per their own release-note conventions — e.g. v1.12.0's TURN change) land roughly every 1–2 months (v1.10.0 → v1.11.0 → v1.12.0 → v1.13.1 spans 2026-03-23 to 2026-06-08, ~2.5 months for 3 minor bumps).
- LiveKit explicitly uses release notes (not a separate deprecation doc) to flag "ATTENTION" breaking changes one minor version ahead of removal — observed pattern: v1.12.0's release notes state "This release maintains backwards compatibility. However, backwards compatibility will be removed in the next release," and that removal did land in the very next minor (v1.13.1). This is the closest thing to a documented deprecation policy: **one-minor-version grace window**, called out inline in release notes rather than a separate changelog/policy doc.

## Proposed Standing Version-Currency Policy

Given the ~2–6 week patch cadence and the inline "one-minor-version grace" deprecation pattern:

1. **Check cadence:** review `livekit/livekit`, `livekit/client-sdk-js`, `livekit/track-processors-js`, and `livekit/egress` (if adopted) release pages monthly — patch releases are frequent enough that a strict per-release bump is unnecessary, but a month-long gap risks missing a breaking-change grace window.
2. **Tolerance for staying behind:** treat being within the *current minor* release train as green (e.g. any v1.13.x while v1.13.x is latest); treat being one minor behind as yellow (plan the bump within the cycle); two or more minors behind as red, since LiveKit's grace window for breaking changes is only one minor version — by the time we're two minors behind, a breaking change may already be forced with no soft-landing release to test against.
3. **Upgrade cadence:** bump the server + client together on a roughly bimonthly cadence, always reading the intervening release notes in full for "ATTENTION"-tagged entries (this is LiveKit's convention for flagging breaking changes) before bumping past them.
4. **Process:** since this chart is single-replica with no HA fallback (`validations.yaml` enforces `replicaCount == 1`), every server bump is a full-service blip — schedule during low-traffic maintenance windows per the existing `terminationGracePeriodSeconds` comment in `deployment.yaml`, and treat any TURN-related "ATTENTION" release note as requiring a values.yaml diff review even while `turn.enabled: false`, since flipping it on later must not be the first time those settings are read.

## Sources

- [livekit/livekit releases](https://github.com/livekit/livekit/releases) — server release list, dates, changelogs (v1.11.0 through v1.13.4)
- [github.com/livekit/livekit/pull/4539](https://github.com/livekit/livekit/pull/4539) — TURN auth backwards-compat removal (referenced from v1.13.1 release notes)
- [livekit/client-sdk-js releases](https://github.com/livekit/client-sdk-js/releases) — JS SDK release list and changesets (v2.19.1 through v2.21.0)
- [livekit/track-processors-js releases](https://github.com/livekit/track-processors-js/releases) — confirms v0.7.2 is latest
- [livekit/egress releases](https://github.com/livekit/egress/releases) — egress release list (v1.11.0–v1.13.0)
- [docs.livekit.io/home/self-hosting/deployment](https://docs.livekit.io/home/self-hosting/deployment/) — self-hosting config reference (TURN/RTC/Redis sections)
- [github.com/livekit/livekit/blob/master/config-sample.yaml](https://raw.githubusercontent.com/livekit/livekit/master/config-sample.yaml) — canonical config key reference (`ttl_seconds`, `allow_restricted_peer_cidrs`, `deny_peer_cidrs`, `advertise_internal_ip`, `skip_external_ip_validation`)
- In-repo: `infrastructure/waddle.cloud/charts/livekit-sfu/Chart.yaml`, `values.yaml`, `templates/configmap.yaml`, `templates/deployment.yaml`, `templates/rtc-nodeport-service.yaml`, `templates/turn-service.yaml`, `templates/turn-udp-nodeport-service.yaml`, `templates/validations.yaml`
- In-repo: `chat/package.json`
