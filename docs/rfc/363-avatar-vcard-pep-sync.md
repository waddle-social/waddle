# RFC 363: Avatar and vCard PEP Sync

**Issue:** [#363](https://github.com/waddle-social/waddle/issues/363)
**Status:** Design ratified, implementation pending
**Follow-ups:** [#435](https://github.com/waddle-social/waddle/issues/435) (workspace roster bridge), [#436](https://github.com/waddle-social/waddle/issues/436) (XMPP-native user deletion), [#437](https://github.com/waddle-social/waddle/issues/437) (binary-payload object storage)
**Date:** 2026-05-09

## Summary

Make profile and avatar updates reliable across web, native, and standard XMPP clients using XMPP-native PEP/vCard flows. The user-visible symptom motivating this work: avatars don't appear in the chat client for other users, especially in DM views and member lists. The root causes span both the server (no bridge from `users.avatar_url` to PEP/vCard) and the client (no PEP push handler, iterate-and-pull misses DM peers).

The design is **push-driven**, not pull-driven. Avatars and display names propagate via XEP-0163 §3 PEP fan-out triggered by presence exchange. The chat client advertises `+notify` filters in CAPS and receives `<message><event>` notifications from the server; it does not iterate JID lists and pull avatars by IQ.

## Problem

Today:

- Self avatar comes from `/api/auth/session.avatar_url` in `chat/src/shell/chat-app-controller.ts:504`.
- Other users' avatars are loaded with `avatar_url: null` (`chat/src/lib/xmpp/client.ts:819`); the chat then calls `fetchUserAvatar(jid)` (`chat/src/lib/xmpp/client.ts:828`).
- `fetchUserAvatar` calls the Rust XMPP avatar path, which tries XEP-0084 PEP first then XEP-0054 vCard fallback (`server/crates/waddle-xmpp-client/src/avatar.rs:214`).
- Server-side PEP avatar reads only from `pubsub_storage` (`server/crates/waddle-server/src/server/routes/websocket/handlers/iq/pubsub_dispatch.rs:184`).
- Server-side vCard reads only from `vcard_storage` (`server/crates/waddle-server/src/server/routes/websocket/handlers/iq/vcard_private.rs:33`).
- `get_user_avatar_url()` exists at `server/crates/waddle-server/src/server/xmpp_profile_state.rs:41` and is wired into the `XmppState` trait but **has zero callers under `server/crates/waddle-server/src/server/routes/`**. Dead at the route layer.
- Client `avatarLookupCandidates` consumes `messaging.messages.value` only (`chat/src/shell/chat-app-controller.ts:496-503`), so DM peers from `dmMessaging.messages.value` never trigger a fetch.
- Workspace members are merged into `avatarUrlByAuthor` with their cached `member.avatar_url` (which is `null` from `list_room_members`); the watch at `chat-app-controller.ts:547-568` only iterates `avatarCandidates`, so workspace members who aren't active room authors never trigger a fetch either.

Net effect: a user resolves their own avatar via the REST session response; everybody else shows blank.

## Design overview

### Push, not pull

XEP-0163 §3 specifies presence-driven PEP fan-out: when a user publishes an item, the server delivers `<message><event>` to every entity that (a) has roster `subscription = from/both` to the publisher AND (b) has cached CAPS advertising `+notify` for the node. There is no explicit pubsub subscription record involved — roster + CAPS is the filter.

The server already advertises `urn:xmpp:avatar:metadata+notify` (`server/crates/waddle-xmpp-core/src/disco/info/features.rs:267`) and `pubsub#auto-subscribe` (`features.rs:258`), and the typed `CapsCache` exists at `server/crates/waddle-xmpp/src/xep/xep0115.rs:114`. The disco/CAPS advertisement currently does not match runtime behavior because the inbound presence handler doesn't wire the `<c hash ver node>` element through the cache, and the publish/fanout path doesn't filter by per-resource CAPS. This RFC closes both gaps.

### XMPP-native, no `url` shortcut

XEP-0084 §4 defines the data node as the authoritative retrieval path for avatar bytes. The optional `url` attribute on `<info>` is a permitted HTTP shortcut, but using it propagates an out-of-band retrieval expectation to every client. We omit `url` and serve everything from the data node, keeping retrieval entirely in-band.

### Conformant SHA-1 from fetched bytes

XEP-0084 §4.1.1 requires the metadata `<info id="...">` attribute to be the SHA-1 of the actual image bytes — not a hash of the URL string. The OIDC bridge fetches the bytes once at publish time, computes the real SHA-1 via the existing typed primitive (`HashValue` from `server/crates/waddle-xmpp/src/xep/xep0300.rs`), and uses `HashValue::to_hex()` as the item id.

### Mirror to legacy and modern vCard

XEP-0398 §3 requires server-side consistency between the PEP avatar and the XEP-0054 vcard-temp PHOTO so legacy clients (XEP-0153) stay in sync. We additionally mirror to XEP-0292 vCard4 (`urn:xmpp:vcard4` PEP node) so modern clients have first-class vCard4 access. Display name (FN) flows alongside PHOTO into both vCard surfaces.

### Independent PHOTO/FN sync

OIDC claims `picture` and `name` have independent provenance. The publish helper handles them as independent flows — either subset can run alone, both can run together, neither runs as no-op.

## Detailed design

### Typed helper signature

```rust
// server/crates/waddle-server/src/<profile module>/mod.rs
pub async fn ensure_pep_profile_published(
    state: &XmppState,
    jid: &BareJid,
    source: ProfileSource,
) -> Result<(), ProfileSyncError>

pub enum ProfileSource {
    Oidc {
        avatar_url: Option<url::Url>,        // typed URL, not &str
        display_name: Option<String>,        // boundary type for free-form unicode
    },
    // Future variants out of scope: User { ... }, Scim { ... }, etc.
}
```

`url::Url` rather than `&str` honors the typed-payloads hard rule. Parsing happens once at the OIDC boundary, never re-parsed downstream. `ProfileSource::Oidc { None, None }` short-circuits as a no-op.

### Wiring into OIDC login

Call sites:

- `server/crates/waddle-server/src/auth/identity.rs:262-269` (`provision_new_user` — new user insert)
- `server/crates/waddle-server/src/auth/identity.rs:329-337` (`reconcile_existing_user` — existing user update)

The reconcile path runs the helper only when `existing.avatar_url != claims.avatar_url` OR `existing.display_name != claims.name`. The helper internally decides which subset of steps to run based on what's set/changed.

The OIDC login response returns immediately. The helper runs in a background `tokio::spawn` task so a slow CDN or unreachable avatar URL does not stall login.

### Outbound HTTP fetch policy

The OIDC `picture` claim is user-controlled in many real-world IdPs (notably GitHub OAuth). SSRF defense is mandatory.

| Knob | Setting | Rationale |
|---|---|---|
| Scheme | `https://` only | Reject `http://`, `data:`, `file:`. |
| SSRF block | After DNS resolution, refuse RFC 1918, link-local (incl. `169.254/16` for AWS/GCP metadata), loopback, non-global IPv6. Re-resolve before connect (DNS rebinding defense). | Closes SSRF without a brittle domain allowlist. |
| Size cap | **100 KB raw (transitional).** Lifts to 1 MB after [#437](https://github.com/waddle-social/waddle/issues/437) ships and binary-payload object storage is available. | Fits comfortably in `pubsub_items.payload_xml TEXT` on D1 (~135 KB serialized). Typical OIDC avatars (GitHub/Google/Bluesky thumbnails) are well under this. |
| MIME allowlist | `image/png`, `image/jpeg`, `image/gif`, `image/webp` | Reject anything else; log and skip publish. |
| Timeouts | 5s connect, 10s total | Short — background bridge, not a critical path. |
| Retries | 1 on transient (5xx, connect timeout, network error). No backoff. | Background bridge; failure cache handles permanence. |
| Failure cache | `users.last_avatar_fetch_attempt_at` + typed `users.last_avatar_fetch_error` (`permanent_4xx | transient_5xx | timeout | mime_rejected | size_exceeded | ssrf_blocked | network`) | Skip re-attempt for 24h after a 4xx; retry sooner after transient failures. |

### Publish chain (set path)

The chain is XEP-prescribed and conditional. PHOTO steps run only if `users.avatar_url` is present; FN steps run only if `users.display_name` is present.

| Step | XEP | Action |
|---|---|---|
| 1 | XEP-0084 §4.1.1 | **(PHOTO)** Fetch URL bytes per the policy above. |
| 2 | XEP-0300 | **(PHOTO)** `compute_hash(HashAlgo::Sha1, &bytes) -> HashValue`. Item id is `HashValue::to_hex()`. |
| 3 | XEP-0084 §4.1.1 | **(PHOTO)** Publish to `urn:xmpp:avatar:data` (item id = SHA-1, payload = base64-encoded bytes). No fan-out — data node has no `+notify`. |
| 4 | XEP-0084 §4.1.2 | **(PHOTO)** Publish to `urn:xmpp:avatar:metadata` with `<metadata><info id="<sha1>" type="<mime>" bytes="<len>"/></metadata>` — exactly one `<info>`, no `url`. Triggers `pubsub_fanout::fan_out_publish` so XEP-0163 §3 push fires. Subscribers immediately follow up with a data-item IQ get and find the data already in storage from step 3. |
| 5 | XEP-0398 §3 / XEP-0054 | **(PHOTO and/or FN)** Read-modify-write vcard-temp via `VCardStore::get(jid)` → mutate → `VCardStore::set(jid, &elem)`. Replace/insert `<PHOTO>` if PHOTO ran; replace/insert `<FN>` if FN sync is in scope. Preserve all other elements (EMAIL, NICKNAME, NOTE, custom). |
| 6 | XEP-0292 / XEP-0163 | **(PHOTO and/or FN)** Read-modify-write XEP-0292 vCard4 PEP item via `pubsub_storage.get_items(jid, "urn:xmpp:vcard4", Some(1), &[])`. Replace/insert `<photo><uri>data:image/png;base64,...</uri></photo>` if PHOTO ran; replace/insert `<fn><text>...</text></fn>` if FN. Preserve other fields. Publish back to `urn:xmpp:vcard4`; fan-out fires for `urn:xmpp:vcard4+notify` subscribers. |
| 7 | XEP-0153 §4 | **(PHOTO only)** Emit fresh self-presence broadcast carrying `<x xmlns="vcard-temp:x:update"><photo>SHA1</photo></x>`. SHA-1 matches the bytes in vcard-temp from step 5. Skipped if PHOTO step did not run. |

**Why this order?** XEP-0084 §4.1.2 opens with: *"Once the data is published, the user publishes the metadata..."* — explicit XEP prescription. Metadata is what triggers fan-out; data must be in storage first so subscribers' immediate follow-up retrieve succeeds. Otherwise contacts get `<item-not-found/>` per XEP-0060 §5.5 and interpret it as a broken avatar.

### Removal chain (XEP-0084 §4.3)

When OIDC re-login arrives with `avatar_url = None` and the user previously had an OIDC-sourced avatar:

- **Guard:** `avatar_source = 'oidc'` AND a previous metadata item exists. If `avatar_source = 'user'`, PHOTO removal is skipped — OIDC's "no avatar" view does not override a deliberate self-publish.
- **Steps:**
  1. Publish empty `<metadata xmlns="urn:xmpp:avatar:metadata"/>` to `urn:xmpp:avatar:metadata` with item id `current` per XEP-0084 §4.3 example. Triggers fan-out; subscribers receive `<message><event>` and drop their cached avatars.
  2. RMW vcard-temp: remove `<PHOTO>`. Other fields untouched.
  3. RMW vCard4 PEP item: remove `<photo>`. Other fields untouched.
  4. Self-presence broadcast carrying empty `<x xmlns="vcard-temp:x:update"><photo/></x>` per XEP-0153 §4.2.
  5. Optionally retract the orphaned `urn:xmpp:avatar:data` item (storage cleanup; not required by XEP).

**FN removal mirror:** when `users.display_name` transitions to None, RMW removes `<FN>` from vcard-temp and `<fn>` from vCard4. No guard — FN has no user-self-managed analog.

**Trigger discipline:** removal fires only on actual transition (Some → None) when a published item exists. Repeated OIDC re-logins with `avatar_url = None` after the removal already fired are idempotent skips. Fetch failure (per the policy above) is **not** a removal — `claims.avatar_url = Some(url)` with a fetch error leaves PEP state alone.

### Schema additions

```sql
ALTER TABLE users ADD COLUMN avatar_source TEXT NOT NULL DEFAULT 'oidc';
-- typed enum on the Rust side: AvatarSource { Oidc, User }
ALTER TABLE users ADD COLUMN last_avatar_fetch_attempt_at TEXT;  -- RFC 3339
ALTER TABLE users ADD COLUMN last_avatar_fetch_error TEXT;       -- typed error enum serialized
```

The XEP-0084 publish handler in `pubsub_dispatch.rs` flips `avatar_source` to `'user'` when a publishing JID matches the target_jid of `urn:xmpp:avatar:metadata` (i.e., the user is publishing their own avatar via XEP-0084 from a client). The OIDC bridge respects this guard.

### Access control

On first publish, set `NodeConfig::public()` on:

- `urn:xmpp:avatar:metadata`
- `urn:xmpp:avatar:data`
- `urn:xmpp:vcard4`

`AccessModel::Open` matches XEP-0084 / XEP-0292 design (avatars and profile data are semi-public) and means any authenticated requester can read. "Unauthorized" then means unauthenticated, already blocked at the websocket auth layer.

`send_last_published_item = OnSub` (already the default in `NodeConfig` at `server/crates/waddle-xmpp-core/src/pubsub/node.rs:153`) ensures new subscribers get the current item pushed immediately.

vcard-temp authorization is already consistent: `vcard_private.rs:33-65` lets any authenticated user read any user's vCard, and `vcard_private.rs:67-76` restricts writes to the owner. No changes needed to the vcard-temp IQ handler.

### XEP-0115 CAPS resolution wire-up

The typed `CapsCache` exists in `waddle-xmpp` but is not wired into the server. This RFC adds:

- **Inbound presence handler:** parse `<c xmlns="http://jabber.org/protocol/caps" hash="..." node="..." ver="..."/>` to typed `EntityCaps { node, hash, ver }`.
- **Cache lookup:** if `(node, ver)` is in `CapsCache`, record session-scoped `(full_jid → caps_ver)` mapping. If not, issue typed `disco#info` query to the resource's full JID, validate response hash matches `ver` (XEP-0115 §5), insert into `CapsCache`, then record the mapping.
- **Disconnect cleanup:** drop the resource→caps mapping. The hash-keyed cache itself stays warm for cross-session reuse.

All caps storage uses typed `EntityCaps`, `Identity`, `Feature`. No stringly-typed feature lists at any boundary.

### XEP-0163 §3 fan-out

Extend `pubsub_fanout::fan_out_publish` (after the existing iteration over explicit pubsub subscribers):

1. Iterate roster contacts of the publisher (`subscription = from/both`).
2. For each contact, look up online resources.
3. For each resource, look up its caps hash from the session-scoped mapping.
4. Look up the feature list in `CapsCache`.
5. If features include the matching `+notify` filter for the published node, send `<message><event>` to that full JID.

**Per-resource semantics:** only resources with the matching `+notify` receive the event. Resources without it receive nothing. Resources mid-resolution (disco#info in flight) are skipped for this publish; they pick up the current item via `send_last_published_item = OnSub` on their next presence broadcast.

### Startup backfill

One-shot migration on server boot:

```sql
SELECT id, xmpp_localpart, avatar_url, display_name, avatar_source FROM users
WHERE (avatar_url IS NOT NULL AND avatar_source = 'oidc')
   OR display_name IS NOT NULL
   OR (avatar_url IS NULL AND avatar_source = 'oidc' AND <a published metadata item exists>)
```

For each row, call `ensure_pep_profile_published`. Bounded concurrency (4 in-flight fetches), continue-on-error, marker recorded in `schema_migrations` to prevent re-run. Failed users are eligible for retry on next startup or via a CLI subcommand. Idempotent per-step (skip if PEP item / vCard field already matches).

### Failure modes and idempotence

The chain is sequential and each step commits independently:

- Step 3 succeeds, step 4 fails → orphaned data item, harmless (no advertising metadata, nobody resolves it).
- Step 4 succeeds, step 5 fails → PEP avatar is correct, vcard-temp PHOTO is stale; XEP-0153 hash advertisement reflects the stale state until next publish. PEP-aware clients see the correct avatar.
- Step 5 succeeds, step 6 fails → vCard4 PEP subscribers get the old item; legacy vcard-temp consumers see the right one.
- Re-running the helper completes idempotently.

**Empty-bytes guard:** the helper refuses to spuriously publish empty metadata when fetched bytes are empty. XEP-0084 §4.3 reserves the empty `<metadata/>` shape for the explicit removal flow above.

## Client design

### WASM client outbound CAPS

Add to the WASM client's outbound CAPS (`server/crates/waddle-xmpp-client/src/`):

- `urn:xmpp:avatar:metadata+notify` (already advertised server-side, must match)
- `urn:xmpp:vcard4+notify` (new — needed for FN updates to push)

Adding new `+notify` filters changes the CAPS verification string and therefore the CAPS hash. Other servers' CAPS caches will re-resolve once on next presence — harmless but worth noting in the PR description.

### Typed PEP event handler

Add to `chat/src/lib/xmpp/client.ts` a typed handler dispatching on event node:

- `urn:xmpp:avatar:metadata`: extract typed `<info id type bytes/>`, IQ-retrieve `urn:xmpp:avatar:data` items by id from the publisher, decode bytes, render as `data:` URL, update `fetchedAvatarUrlByJid[publisherJid]`.
- `urn:xmpp:vcard4`: extract typed `VCard4`, surface FN as the displayed name for the publisher.
- On receipt of a push for either node, the JID is added to a "push-served" set so subsequent iterate-and-pull skips it.

### Transitional iterate-and-pull

This RFC ships before [#435](https://github.com/waddle-social/waddle/issues/435) (workspace roster bridge). Until #435 lands, push delivery only reaches roster-explicit contacts; workspace colleagues need the iterate-and-pull fallback.

So this RFC **adds the push path but does not remove iterate-and-pull**:

- Fix the original observed gap: extend `avatarLookupCandidates` at `chat/src/shell/chat-app-controller.ts:496-503` to also consume `dmMessaging.messages.value` so DM peers are queued for IQ pull.
- The watch at `chat/src/shell/chat-app-controller.ts:547-568` stays in place but becomes a fallback: triggers IQ pull only for JIDs not already served by push.
- After #435 ships, a small follow-up PR removes the watch entirely.

## XEPs and conformance considerations

- **XEP-0060 PubSub** — base mechanism for publish/retrieve/notify.
- **XEP-0163 PEP** — auto-discovery via CAPS, presence-driven fan-out (§3).
- **XEP-0084 User Avatar** — data and metadata nodes (§4.1), `url` is optional and we omit it, removal via empty metadata (§4.3).
- **XEP-0292 vCard4 Over XMPP** — modern profile in PEP at `urn:xmpp:vcard4`.
- **XEP-0153 vCard-Based Avatars** — presence advertises SHA-1 hash of vCard PHOTO bytes (§4); empty `<photo/>` signals "no avatar" (§4.2).
- **XEP-0398 PEP/vCard Conversion** — server-side consistency between PEP avatar and vCard PHOTO (§3).
- **XEP-0054 vcard-temp** — legacy vCard storage; remains the source for XEP-0153.
- **XEP-0115 Entity Capabilities** — CAPS hash advertisement and verification (§5).
- **XEP-0300 Cryptographic Hashes** — typed `HashValue`/`HashAlgo` primitive.

## Out of scope

- **Synthesize-on-read fallback.** Rejected as non-conformant — would require a fake SHA-1 id and would never fire PEP notifications.
- **Including `url` attribute in published metadata.** Rejected to keep retrieval entirely XMPP-native.
- **Per-render lazy fetch in the client as the primary mechanism.** Replaced by the push-based design; lazy is fallback only.
- **Federated peers (s2s avatar push, remote CAPS resolution).** s2s subsystem doesn't yet exist (see `server/crates/waddle-server/src/server/routes/interpret/route_to_connection.rs:31,104` — cross-domain JIDs are dropped). Code is written route-agnostically (typed `Jid`, dispatch through existing routing) so explicit federation tests can be added when s2s lands.
- **Workspace roster bridge** — tracked in [#435](https://github.com/waddle-social/waddle/issues/435).
- **Conformant user-deletion semantics** (XEP-0084 §4.3 empty publish + RFC 6121 §3.4 presence-unavailable + roster `subscription="remove"` cascade) — tracked in [#436](https://github.com/waddle-social/waddle/issues/436). This RFC verifies FK cascade so writes don't orphan rows; the wire effects on deletion are #436's job.
- **Binary-payload object storage** — tracked in [#437](https://github.com/waddle-social/waddle/issues/437). Until that lands, the avatar fetch policy enforces a 100 KB inline cap.
- **EMAIL, NICKNAME, ORG, and other vCard fields** — only PHOTO and FN are populated by the OIDC bridge in this RFC.
- **Per-user opt-out of presence/avatar sharing within a workspace** — workspace-level settings concern, not in this RFC.
- **Periodic re-fetch of unchanged URLs** to catch upstream content changes — login-only fetch is sufficient.
- **Concurrency control on vCard writes** — last-write-wins for now; typed concurrency is a separate concern, filed if it bites in practice.

## Implementation plan (PR breakdown)

This RFC lands as 6 sequential PRs. Each builds on the previous and ships in a working, independently-testable state. No feature flags (per the project's no-backwards-compat rule).

| # | PR title | Scope | Working state at end |
|---|---|---|---|
| 1 | `feat(server): wire XEP-0115 CAPS resolution into presence handler` | Inbound presence handler parses `<c hash ver node>`, resolves via `CapsCache` (hit) or disco#info (miss with hash verification per XEP-0115 §5), records per-resource caps mapping; disconnect cleans up the mapping while leaving the hash-keyed cache warm. New `xep0115_caps_ws.rs`. | CAPS conformance tests pass. No fan-out behavior change yet (no consumer). |
| 2 | `feat(server): XEP-0163 §3 PEP fan-out by roster + CAPS` | Extend `pubsub_fanout` to iterate roster `from/both` contacts of the publisher, consult per-resource cached CAPS, deliver `<message><event>` to matching resources. Per-resource semantics. Updates `xep0163_pep_ws.rs`. | Existing PEP nodes (mood, activity, microblogging, nick, tune) now fan out per XEP-0163 §3. Avatars not yet involved. |
| 3 | `feat(server): typed ProfileSource + OIDC avatar/FN publish chain` | Adds typed `ProfileSource` enum, `ensure_pep_profile_published` happy-path branches (steps 1–7 for set), `users.avatar_source` schema column, fetch policy with SSRF + 100 KB cap + MIME allowlist + retry + failure cache columns, `NodeConfig::public()` on the three nodes. Wire into OIDC `provision_new_user` and `reconcile_existing_user` (background `tokio::spawn`, login non-blocking). New `xep0084_avatar_ws.rs` covering the set path + vCard mirror RMW + vCard4 PEP push + presence refresh. Updates `xep0054_0049_0191_ws.rs` and `rfc6121_presence_ws.rs`. New `xep0084_avatar.rs` core-protocol unit tests. | OIDC login publishes a conformant avatar + FN; subscribers receive push (works because PRs 1+2 already landed); user-managed avatar guard works. No removal yet, no backfill. |
| 4 | `feat(server): XEP-0084 §4.3 avatar removal + FN removal mirror` | Adds removal branches to `ensure_pep_profile_published`: empty `<metadata/>` publish per XEP-0084 §4.3, vCard PHOTO/FN removal RMW, empty `<photo/>` presence per XEP-0153 §4.2. User-managed avatar exception preserved. Extends `xep0084_avatar_ws.rs` with removal tests. | OIDC re-login with `picture` claim disappearing fires the conformant removal flow; subscribers' caches invalidate. |
| 5 | `feat(server): startup backfill for OIDC profile sync` | One-shot startup migration with bounded concurrency (4 in-flight fetches), idempotence marker in `schema_migrations`, failure cache columns. Backfill query covers set, FN-only, and removal-needed cases. Continue-on-error. CLI subcommand for retry. Extends `xep0084_avatar_ws.rs`. | Existing users with `users.avatar_url` set get conformant PEP/vCard state on next server boot; OIDC-sourced no-avatar transitions get the conformant removal applied. |
| 6 | `feat(chat): PEP event handler + DM peer iterate-and-pull fix` | WASM client outbound CAPS adds `urn:xmpp:avatar:metadata+notify` and `urn:xmpp:vcard4+notify`. Chat client adds typed PEP event handler dispatching on node — avatar metadata triggers data IQ retrieve and updates `fetchedAvatarUrlByJid`; vCard4 surfaces FN as displayed name. Push-served JIDs added to a set so iterate-and-pull skips them. DM peer extension: `avatarLookupCandidates` consumes `dmMessaging.messages.value`. | Avatars resolve in chat for roster contacts (push) and DM peers (pull); workspace colleagues continue using pull until #435 lands. After #435, a small follow-up PR removes the iterate-and-pull mechanism. |

## Test strategy

All server-side tests follow the project's WebSocket-driven XEP conformance pattern (`server/crates/waddle-server/tests/*_ws.rs`). Per the CLAUDE.md hard rule, every implemented XEP must have a dedicated Rust custom test suite — this RFC adds one for XEP-0084 and extends the existing XEP-0163, XEP-0054, and RFC 6121 suites.

| New / Edit | Path | Coverage |
|---|---|---|
| New | `server/crates/waddle-server/tests/xep0084_avatar_ws.rs` | `ensure_pep_profile_published` happy path; XEP-prescribed publish order (data before metadata, fan-out fires only after metadata); OIDC re-login no-op when URL unchanged; OIDC re-login republish when URL changed; user self-publish flips `avatar_source = 'user'` and protects subsequent OIDC re-login; XEP-0084 §4.3 removal flow; user-managed exception during removal; FN-only sync; FN removal mirror; failure-mode idempotence; startup backfill; access control (open read for any authenticated requester, forbidden cross-user publish with typed stanza error); restart persistence; vCard mirror RMW preserves user-set fields. |
| Edit | `server/crates/waddle-server/tests/xep0163_pep_ws.rs` | XEP-0163 §3 fan-out: roster + CAPS filter; per-resource semantics; multi-resource case (one resource with `+notify`, one without — only the first receives). |
| New | `server/crates/waddle-server/tests/xep0115_caps_ws.rs` | Presence with `<c hash ver node>` triggers cache lookup or disco#info; hash verification rejects mismatched responses (XEP-0115 §5); cache hit avoids the disco query; multi-resource independent tracking; disconnect cleans up resource→caps mapping while leaving the hash-keyed cache warm. |
| Edit | `server/crates/waddle-server/tests/xep0054_0049_0191_ws.rs` | PEP avatar publish mirrors to vcard-temp PHOTO and FN; vCard4 PEP carries equivalent photo (data: URI) and FN; vCard4 publish triggers fan-out; RMW preserves user-set fields. |
| Edit | `server/crates/waddle-server/tests/rfc6121_presence_ws.rs` | After avatar publish, the user's next presence broadcast carries `<x xmlns="vcard-temp:x:update"><photo>SHA1</photo></x>` matching the published bytes. After avatar removal, presence carries empty `<photo/>`. |
| New | `server/crates/waddle-xmpp/tests/xep0084_avatar.rs` | Core-protocol unit tests: metadata stanza shape, `<info>` attribute requirements (id required, type required, bytes required, no `url` emitted), data item base64 round-trip, parser rejects malformed metadata. |

### Client coverage

- PEP event handler updates `fetchedAvatarUrlByJid` when a `urn:xmpp:avatar:metadata` event arrives, then IQ-retrieves data and renders a `data:` URL.
- vCard4 event handler surfaces FN as displayed name.
- Iterate-and-pull fallback at `chat-app-controller.ts:547-568` fires only for JIDs not already push-served.
- DM open queues the peer for IQ pull via the extended `avatarLookupCandidates`.
- `fetchUserAvatar(jid)` fallback resolves for stranger JIDs on demand.

### Validation

- `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt`.
- `bun test && bun run lint` (knip clean).

## Acceptance criteria

- A user can publish a profile/avatar update and another subscribed/contact client receives it without reload-only behavior — delivered via PEP push.
- A fresh client retrieves the last published avatar/profile through the XMPP path; first presence exchange triggers `send_last_published_item = OnSub` delivery.
- A user who has only ever set their avatar via OIDC login (never via XMPP publish) is resolvable by other users through XEP-0084 — the OIDC bridge materializes a real conformant PEP item with SHA-1 derived from fetched bytes.
- vCard fallback (XEP-0054) and PEP avatar metadata/data stay consistent; XEP-0153 presence-hash matches published bytes.
- A user who self-published a custom avatar via XEP-0084 is not overwritten by subsequent OIDC re-login.
- DM views show peer avatars without opening a profile or visiting a shared room first. While #435 is pending, satisfied by iterate-and-pull on `dmMessaging.messages`. After #435, satisfied by push.
- Workspace member listings show member avatars. While #435 is pending, satisfied by iterate-and-pull. After #435, satisfied by push.
- Unauthorized cross-user publishes are rejected with typed stanza errors. Reads on the open avatar nodes succeed for any authenticated requester.
- Disco/advertised support matches runtime behavior — `urn:xmpp:avatar:metadata+notify`, `urn:xmpp:vcard4+notify`, `pubsub#auto-subscribe` all advertised AND functional.
- Inbound presence with `<c xmlns="http://jabber.org/protocol/caps">` populates `CapsCache` (hit) or triggers typed disco#info with hash verification (miss). Resource→caps mappings recorded on presence, dropped on disconnect.
- A PEP publish from user Y is delivered as `<message><event>` to exactly the online resources of Y's roster `from/both` contacts whose cached CAPS include the matching `+notify` filter.
- Per-resource semantics: contacts with multiple resources only see push on resources advertising `+notify`. Resources mid-resolution are skipped for this publish but pick up via `OnSub` later.
- Avatar removal: `users.avatar_url` Some → None for `avatar_source = 'oidc'` users publishes empty `<metadata/>`, removes PHOTO from vCards, emits empty `<photo/>` presence. User-managed avatars are unaffected.
- FN removal: `users.display_name` → None removes `<FN>` from vcard-temp and `<fn>` from vCard4; vCard4 fan-out fires.

## Cross-references

- **Issue:** [#363](https://github.com/waddle-social/waddle/issues/363)
- **Followed by:** [#435](https://github.com/waddle-social/waddle/issues/435) (workspace roster bridge — required for push delivery to workspace co-members; until it lands, iterate-and-pull covers them)
- **Touched by:** [#436](https://github.com/waddle-social/waddle/issues/436) (conformant XMPP-native user deletion semantics — wire effects on deletion live there)
- **Unblocked by:** [#437](https://github.com/waddle-social/waddle/issues/437) (binary-payload object storage — lifts the 100 KB transitional cap)
- **Parent backlog:** [#208](https://github.com/waddle-social/waddle/issues/208), [#212](https://github.com/waddle-social/waddle/issues/212), [#215](https://github.com/waddle-social/waddle/issues/215)
