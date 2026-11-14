# Issue 2047 — plan v3: open XMPP and Spaces federation

This is a delivery plan; it establishes no implementation, interoperability or
deployment readiness. Architecture decisions D1–D9 and gates G1–G22 are
recorded in
[ADR-0020](../../server/docs/adrs/0020-independent-community-federation.md);
they bind every slice below. Plan v3 replaces plan v2.

## Outcome and invariants

A user signed in to a Waddle home deployment uses the same home account and
client session in communities hosted by other Waddle deployments and by
independently operated XMPP servers. Users homed on Prosody, ejabberd and other
compatible servers join suitably configured Waddle rooms and, where their
software supports it, navigate Waddle Spaces. A compatible peer needs no Waddle
software, AT Protocol account, shared database or bilateral registration.

- The home authenticates the account and its bound resources; the resource
  host alone authorizes its resources. Normalized bare JIDs identify accounts
  and grants; full JIDs carry resource and routing provenance.
- Remote accounts hold only non-privileged grants on a host (D8), so a
  community is administered by accounts homed on the deployment that hosts it.
- Presence and direct messages between accounts on different domains are in
  scope (D7).
- S2S is off by default, and nothing is advertised until verification
  demonstrates it. Enabling S2S in a deployment is a separate decision with a
  named window and runbook.
- A Space subscription is not a room grant. Every accessible child room is
  reachable by its plain MUC JID.
- PostgreSQL remains the authority for session lifecycle, claims and contiguous
  frontiers. Do not shortcut the distributed-actor roadmap or extend XEP-0198
  into arbitrary-node resume.
- Transport acceptance is not recipient delivery. Do not promise global
  ordering or exactly-once delivery, and never fabricate success.
- Federation authentication is not end-to-end encryption; a host can read the
  content it hosts.
- Out of scope: AT Protocol home login (its own issue and ADR, per D6), account
  linking and home migration, and cross-domain calls, media and push.

## Starting point

At main `f77c03814`:

- There is no S2S transport: startup acquires only the `http` listener.
- Foreign ping, Carbons control, discovery and legacy session requests are
  rejected locally (#2049, #2054, #2063, #2064), and local full-JID IQ replies
  are forwarded (#2070). There is no general destination classifier.
- Production SpiceDB IDs map `@` to `|` and `.` to `=`, which is not injective.
  A `v2_` base64url codec exists only in `server/experiments/authz-id-codec`.
- Grant-fence evidence is test-only (#2050, #2051, #2057, #2060, #2061); no
  production grant authority exists.
- Spaces advertise `urn:xmpp:spaces:0`; the XEP-0503 module header still names
  version 0.2.0.
- Client rooms, Spaces and message IDs are not qualified by authority.
- The GitHub workflow sources are restored (#2395), but GitHub Actions itself
  is still disabled; evidence is recorded by hand in `docs/evidence/2047/`.

## Phases

| Phase | Scope | Slices |
| --- | --- | --- |
| P0 | Architecture decisions, gates and pinned sources | W0 |
| P1 | Peer discovery, authentication and S2S transport | W4a–W4d |
| P2 | Routing, authorization identity and host grant authority | W1, W2, W5i, W5o, W5a–W5e, W5r |
| P3 | MUC, account traffic and client interoperability | W3, W6a–W6c, W8 |
| P4 | Spaces interoperability | W7a, W7b, W8 |
| P5 | Two-deployment and third-party interoperability harness | W10 |

Verification follows the slices; capability advertisement (W11) and the D1
default change (W12) follow verification. AT Protocol home login moves to its
own issue (D6) and depends on W1; it has no slice here.

Earlier documents use plan v2 phase names: F0 is P0 and the design gates; F1 is
W1, W2 and W3; F2 is W4a–W4d and W5a; F3 is W5i, W5o, W5b–W5e and W5r; F4 is W8
with W6 and W7; F5 is W10 and the verification phase.

## Slices

Each slice is one branch and one draft PR, stacked in dependency order, with
Conventional Commits using one scope. Each slice ships its tests compiled but
not executed, except where D9 says otherwise.

| Slice | Phase | Scope | Main files | Depends on |
| --- | --- | --- | --- | --- |
| W0 | P0 | ADR-0020 decisions D1–D9 and gates G1–G22; plan v3; reconcile the design ledgers; pin RFC and XEP sources | `server/docs/adrs/0020-…`, `docs/planning/federation-2047.md`, `server/docs/planning/*-2047.md` | Accepted decisions |
| W1 | P2 | Production authorization ID codec (G9): promote the `v2_` codec into `permissions/`, typed subject and object IDs, writer and reader inventory, empty-namespace proof or audited migration, fail closed on legacy IDs | `permissions/spicedb.rs`, `experiments/authz-id-codec` | — |
| W2 | P2 | Destination classifier before namespace dispatch: missing, local bare, local full, each served component, remote, invalid; generalize the foreign-request rejections; IQ result and error routing; no error loops | `iq/dispatch.rs`, `route_to_connection/implementation.rs`, `server/config.rs` | — |
| W3 | P3 | Client qualified identity: room is the bare room JID, Space is service JID plus node, message IDs are scoped by conversation and authority; caches, drafts, unread, reactions and routes; no cross-community UI yet | `chat/src`, `waddle-client-session`, `waddle-xmpp-client` | — |
| W4a | P1 | `waddle-xmpp::s2s`: bounded streaming XML codec for `jabber:server`, typed stream headers, negotiation state machine, typed stream errors. Decides G3 and its part of G1: malformed-header and stream-error behavior, including the error for a missing pre-TLS `from`, and remaining SASL conformance | new `crates/waddle-xmpp/src/s2s/` | — |
| W4b | P1 | Verifier per the verifier design as amended by D2: rustls client and server verifier traits; per-domain trust profiles, public WebPKI by default and private-CA with hard CRL checks where configured; serverAuth-only EKU accepted in both roles in every profile; DNS-ID, SRV-ID and XmppAddr checks; chain bounds. Decides G2 and its part of G1: errata disposition, TLS 1.2 support (RFC 9325 section 3.1.1), XmppAddr and wildcard policy, IDNA for reference identities, the public root store and public-chain revocation. Focused tests run on completion (D9) | new `crates/waddle-server/src/s2s/verify/` | W0 |
| W4c | P1 | Connection manager: STARTTLS-only `s2s` listener through `LISTEN_FDS` (no inbound direct TLS) with a mandatory pre-TLS `from`; outbound dialer with STARTTLS and XEP-0368 direct TLS; SASL EXTERNAL; `VerifiedPeerStream` carrying authentication strength; D1 admission modes and lists with canonical matching, snapshot or reviewed reload, and closing the streams of a domain no longer admitted; per-peer limits and global peer, socket and handshake caps with eviction and backoff (G7); deadlines; shutdown join. Merges only after the G4 design review | `server/lifecycle.rs`, `main.rs`, new `s2s/conn/`, `s2s/config.rs` | W4a, W4b |
| W4d | P1 | Discovery under D3 (G6): merged SRV ordering, "." targets, A/AAAA fallback only on a negative answer, blocked address classes checked on the address actually connected, target and address caps, configured endpoints, and a stub-resolver suite. Dialback under D2 (G5): inbound behind its own list and TLS, outbound Initiating and Authoritative Server roles over TLS only, XEP-0185 keys, one domain pair per stream, rejection of unsolicited `db:result` and `db:verify`, the `dialback` strength tag, wire profile and active-failure classification. The reviewed change lifting the loopback and `.test` restriction (G6) | new `s2s/discovery.rs`, `s2s/dialback.rs` | W4c |
| W5i | P2 | Identity profile, host-grant B6 beyond the codec (G10): choose the profile; enforce it on XML-decoded identity inputs before lossy JID preparation at C2S parsing, provisioning, authentication and directory lookup; canonical bare-JID account binding; authoritative reconciliation | `waddle-xmpp` frame parsing, `auth/directory.rs`, provisioning paths | W1 |
| W5o | P2 | Outcast precedence as its own behavior change: every channel permission (`read`, `send_message`, `view`, `moderate`, `manage`, `delete`) subtracts `outcast`; fenced schema transition in which startup compares a versioned schema and refuses to overwrite a different one (parts of G14 and G15) | `permissions/schema.rs`, `permissions/actor.rs`, `permissions/spicedb.rs` | W1 |
| W5a | P2 | S2S routing: typed `RemotePrincipal` carrying domain and authentication strength; G10 enforcement at S2S ingress; ingress from-domain validated against the stream; bounded per-peer egress queues with backpressure; IQ correlation with at-most-once completion and expiry, including a reply that reaches another replica; W2's remote branch connected to egress for messages, presence and IQs; federated handoff contract (G8); remote traffic to local accounts refused per D7 until W6c | new `s2s/route/`, `routes/interpret/` | W1, W2, W4c, W5i |
| W5b | P2 | Grant authority part 1, D5's first slice: PostgreSQL canonical store, durable scope and generation, mutation linearization, SQL admission inside the fresh-manifest acceptance transaction for one fresh isolated community, a managed members-only room and explicit affiliations; fenced SpiceDB projection for that relation family; recovery of unknown writes; store capabilities and credential separation (G11); lock order (G12); privileged grants to remote JIDs refused at issuance (D8). Focused tests run on completion (D9) | `permissions/`, new migrations, `ingress/commit/`, `ingress/commit_room.rs` | W0, W1, W5i, W5o, #2046 role and transaction cutover |
| W5c | P2 | Grant authority part 2: fresh writer sweep and fencing of every writer (G11); retire the outcast-overwriting owner reseed, the latent `set_room_affiliation` path and the graph-first MUC admin write; one admission check on joins, protected reads (MAM, disco), PubSub and stream-management fanout (G16), queued work and room administration, with the lock order extended to each surface (G12) | `server/bootstrap_membership.rs`, `spaces_pubsub_seed.rs`, PubSub and MUC writers | W5b |
| W5d | P2 | Revocation: acknowledged fence; non-reuse of retired JIDs on Waddle homes; the operator procedure after a reported third-party reassignment (D8); the #2056 Space-fanout negative control becomes a passing fence test. Focused tests run on completion (D9) | `permissions/`, Space fanout | W5c |
| W5e | P2 | Effect-boundary admission for remote principals (G17): the admission witness binds the remote domain, its authentication strength and the current D1 admission decision; work from a domain no longer admitted is rejected; privileged authority is never honored for a remote principal; host policy can require `pkix` | `permissions/`, admission witness | W5a, W5c |
| W5r | P2 | Restore incarnation, host-grant B7 (G13): an authority-incarnation transition that survives PostgreSQL rollback, invalidation of old credentials and work, verified reconstruction before admission | `permissions/`, startup, new migration | W5b |
| W6a | P3 | Inbound MUC: remote occupants join local rooms (XEP-0045), presence and message fanout over S2S, host-authorized MAM (XEP-0313), disco#info, disco#items and ping for remote askers | MUC handlers, XEP-0045 and XEP-0313 paths | W5a, W5e |
| W6b | P3 | Outbound MUC: local users join remote rooms through C2S-to-S2S presence; traffic from a joined remote room to the occupant's full JID is admitted; self-ping (XEP-0410); occupant cleanup on S2S loss | `route_to_connection`, presence routing | W5a |
| W6c | P3 | Account traffic across S2S (D7, G18): inventory of every local-account handler a remote sender can reach; RFC 6121 server rules for remote messages, presence, subscription requests and probes, and IQs to accounts; local accounts' presence, direct messages and subscriptions to remote contacts; Carbons (XEP-0280) for those messages; replaces W5a's refusal | presence, message, roster and IQ handlers | W5a, W5c, W5e |
| W7a | P4 | Spaces over S2S: XEP-0503 0.3.0 PubSub on remote service JIDs; subscription is not a room grant; no invented inheritance; module header corrected to 0.3.0; the third-party XEP-0503 survey repeated before acceptance is fixed | `xep0503.rs`, PubSub service | W5c, W6a |
| W7b | P4 | MUC fallback: clients without Spaces reach every child room by plain MUC JID, and disco items list them | Spaces and MUC disco | W7a |
| W8 | P3/P4 | Web pilot: remote community discovery and join; XEP-0402 bookmarks for remote rooms; qualified joined-space preferences that are never authority; leave, unsubscribe, forget and revoke kept distinct; outage versus denial versus deletion states; no fetching of Space avatar or banner URLs (D4) | `chat/src` | W3, W6b, W7a |
| W10 | P5 | Harness: two native Waddle processes with separate PostgreSQL, SpiceDB and secrets; pinned Prosody and ejabberd 25.08 or later (`s2s_use_starttls: required`) through Nix, with accounts for presence and direct-message flows; `.test` CA configured as each domain's private-CA profile, including serverAuth-only leaves; loopback configured endpoints; records whether each peer sends a pre-TLS `from` and ejabberd's effective STARTTLS setting | new `server/experiments/federation-interop/`, `flake.nix` | W4c; written during implementation, run in verification |
| W11 | — | Capability advertisement (G21): add only what verification demonstrated, per client and per release; delta review | `capabilities.toml`, disco | Verification and close-out review |
| W12 | — | D1 default change from `allowlist` to `open` with a denylist, once G20's evidence has passed; delta review; #2047 does not close before it | `s2s/config.rs` | Verification and close-out review |

The critical path runs through the grant fence: W5o, W5b, W5c, W5e, W6a and
W7a. W5e also waits for the transport chain (W4a and W4b, then W4c, then W5a).
W5b needs #2046's role and transaction cutover; W1–W4 do not wait for it.

## Rules for every slice

- Follow [`server/AGENTS.md`](../../server/AGENTS.md) and
  [the Rust style conventions](../rust-style.md): no XML through `format!` or
  concatenation; typed stanzas, `jid` types and `xep::*` namespace constants;
  raw bytes only at the TLS and codec boundary.
- No `unwrap()` in first-party production code. Clippy runs with
  `-D warnings`; any new `#[allow]` is justified in the PR.
- XEP behavior that is added or changed updates its dedicated suite in the same
  PR.
- Review the pinned sources before changing wire behavior and record the
  commit in the PR: RFCs from the RFC Editor, XSF sources at xsf/xeps
  `4132c531487fe9450ae90b905b8df950d0d478b2`. `xeps/` is an untracked local
  cache that `cuenv.lock` fills from a different xsf/xeps commit, so fetch the
  pinned commit before relying on it.
- A slice that owns a decision gate records the decision in the named design
  document. Every slice gets an independent review of its diff, including those
  decisions, before it merges into the integration branch; an unresolved
  blocking finding pauses the merge.
- A behavior-changing fix ships only in its own reviewed scope, never as a side
  effect of a dark foundation slice.
- S2S stays off unless configured. Nothing is added to `capabilities.toml` or
  disco before verification, and no Helm, DNS, certificate or production
  change belongs in a slice.
- Metrics use only `counter_add!`, `histogram_record!` and `gauge_record!`,
  never with a JID, domain or stanza ID attribute.
- A change to `RemoteStanzaEnvelope` or the ordered relay types bumps
  `waddle.clustering.relay.deliver_ordered.vN`.
- Tests are native Rust without containers. New server tests go in
  `crates/waddle-server/tests/*.rs`, not the library test crate.
- Adjacent defects are reported as follow-ups instead of widening a slice.

## Static checks during implementation

Under D9, no test runs until every slice is merged into `integration/2047`,
except the focused tests of W4b, W5b and W5d, which run when each of those
slices completes and must pass before slices that build on it merge. Tests are
still compiled, because a test that does not build is a defect found early. Run
each command on its own line from the repository root inside the Nix
development shell, for touched crates only:

```
cargo fmt --manifest-path server/Cargo.toml --all -- --check
cargo clippy --manifest-path server/Cargo.toml -p waddle-xmpp --all-targets --locked -- -D warnings
cargo clippy --manifest-path server/Cargo.toml -p waddle-server --lib --locked -- -D warnings
cargo check --manifest-path server/Cargo.toml -p waddle-server --features test-support --locked --test <each new or changed test file>
git diff --check
```

Chat slices (W3 and W8) also run:

```
bun run --cwd chat lint
bunx --cwd chat astro check
```

Not allowed before verification, apart from D9's exception: `cargo nextest
run`, `cargo test`, `bun test`, any `server/experiments/*/run-native.sh`, the
interoperability harness, and `cargo check --lib --tests` on `waddle-server`.

Each slice lists its unrun tests (file, test names and prerequisites such as
PostgreSQL, SpiceDB, Prosody, ejabberd or two processes) in
`docs/evidence/2047/unrun-tests.md`, which is the verification checklist.
After each round of merges, the integration branch runs these checks once on
the combined tree and is rebased on main.

Slice PRs stay draft until verification. When GitHub Actions is enabled, its
results on slice PRs are recorded in the evidence log, but they do not replace
the verification phase, and a slice merges into main only after verification
and with passing checks at its merge head.

## Gate ownership

The ADR defines each gate's kind and what it blocks. This table names its
owner, the point by which its decision must be made and reviewed, and where its
evidence is discharged. Step numbers refer to the verification phase below.

| Gate | Owner | Decided and reviewed before | Evidence discharged at |
| --- | --- | --- | --- |
| G1 Transport standards profile | W4a (headers, errors, SASL), W4b (errata, TLS versions, identity matrix, IDNA), W4d (dialback wire profile) | Each owner's merge; W4a and W4b parts also before the G4 review | Steps 2 (W4a, W4b and XEP-0178 suites), 6 and 7 |
| G2 Concrete verifier | W4b | W4b merge | W4b focused tests on completion (D9); steps 2 and 6 |
| G3 Codec and restart buffers | W4a | W4a merge | Step 2 |
| G4 Transport design review | Independent reviewer, on W4a and W4b's recorded decisions | W4c merge | Review only |
| G5 Dialback profile and active failures | W4d | W4d merge; enabling inbound dialback | Steps 2 (XEP-0220 suite with XEP-0185 keys), 6 and 7 |
| G6 Discovery profile and the loopback and `.test` restriction | W4d | W4d merge | Step 2 (XEP-0368 stub-resolver suite); step 6 for configured endpoints |
| G7 Admission and resource model | W4c | W4c merge | Steps 6 and 8 |
| G8 Federated handoff contract | W5a | W5a merge | Steps 6, 7 and 8 |
| G9 Authorization ID codec | W1 | W1 merge | Steps 2 and 4 |
| G10 Identity profile | W5i, applied at S2S ingress by W5a | W5i merge; W5a merge | Steps 2, 4 and 6 |
| G11 Writer exclusion, store capabilities, credentials | W5b, W5c | Each owner's merge | W5b focused tests (D9); step 4; old binaries and credentials retired under G22 |
| G12 Lock order and atomic admission | W5b, extended by W5c | Each owner's merge | W5b focused tests (D9); step 4 |
| G13 Restore incarnation | W5r | W5r merge | Step 4 |
| G14 Complete policy | W5b, W5c and W5e per surface; W5o for outcast precedence | Each owner's merge | Steps 2 (W5o suite) and 4 |
| G15 Projection and schema protocol | W5b (projection writes), W5o (schema transition) | Each owner's merge; any slice that would read the projection for admission | Step 4 |
| G16 PubSub, DirectFrame and sink contracts | W5c | W5c merge | W5d focused tests with the #2056 control (D9); step 4 |
| G17 Effect-boundary admission for remote principals | W5e | W5e merge | Steps 6 and 7 |
| G18 Remote traffic to local accounts | W6c | W6c merge | Steps 2 (RFC 6121 account-traffic suite), 6 and 7 |
| G19 Release evidence and close-out review | Verification | — | Steps 6, 7, 8 and 9 |
| G20 D1 default change | W12 | — | Steps 4, 6, 7 and 9, then W12's delta review in step 10 |
| G21 Advertisement | W11 | — | Steps 6 to 9, then W11's delta review in step 10 |
| G22 Activation | Separate owner decision | — | Its runbook, including a check of the public-WebPKI profile against a real public peer |

## Verification phase

Verification runs once, on `integration/2047`, cheapest step first, so a
failure points at one slice before the slow suites start. It needs a host with
at least 16 GB of RAM, PostgreSQL 17, SpiceDB and Nix. Failures are grouped by
slice and fixed on that slice's branch; after a fix, rerun the failing step and
every later step, then merge again. Skipped prerequisites are recorded as
unverified, never as passed.

1. **Static, whole tree:** workspace Clippy with `--all-targets -D warnings`
   (including the `waddle-server` library test crate), `cargo fmt --check`,
   chat lint and `astro check`.
2. **Focused suites in dependency order:** the W1 codec, W2 classifier, W4a
   codec, W4b verifier matrix, W5i identity profile and W5o outcast and schema
   suites; the transport XEP suites for 0178, 0220 (with XEP-0185 keys) and
   0368 (discovery with a stub resolver); then the XEP suites for 0030, 0045,
   0060, 0199, 0280, 0313, 0402, 0410 and 0503, and the RFC 6121
   account-traffic suite.
3. **`waddle-server` library unit tests** (the 6–10 GB build).
4. **Native PostgreSQL and SpiceDB experiments:** grant-fence, room-child,
   parent-move and space-fanout; the W5b and W5d focused tests again on the
   integrated tree; the W5r restore schedule; and the W5o mixed-version schema
   case. The #2056 negative control must now reject the stale delivery; the
   old behavior passing is a W5d regression.
5. **Full workspace:** `cargo nextest run --workspace --all-targets --locked
   --profile ci`, `cargo test --doc --workspace --all-features`, and
   `bun test` in `chat`.
6. **Two independent Waddles** (W10; `academy.test` and `korora.test` with
   separate databases, SpiceDB and secrets, real WSS login and S2S): positive
   flows, then the negative matrix.
7. **Third-party interoperability** with the pinned Prosody and ejabberd, in
   both directions (table below).
8. **Failure and restart:** each side restarted while the other stays up;
   partition; a remote outage shown as distinct from denial; local traffic
   shown to keep flowing.
9. **Close-out:** an independent, read-only adversarial review of the
   integrated head, repeated after any fix until it returns CLEAN. The slices
   then merge into main.
10. **After close-out:** W11 advertises only what steps 6–8 demonstrated, and
    W12 moves the D1 default from `allowlist` to `open` once G20's evidence
    has passed. Each gets an independent delta review of its own change before
    it merges.

| Flow | Waddle ↔ Waddle | Waddle ↔ Prosody | Waddle ↔ ejabberd |
| --- | --- | --- | --- |
| S2S with TLS and SASL EXTERNAL | Both directions | Both directions | Both directions |
| Dialback under the D2 policy | Inbound only for a listed domain; outbound both roles | Inbound only for a listed domain; outbound both roles | Inbound only for a listed domain; outbound both roles |
| Presence subscription and presence between accounts | Both directions | Both directions | Both directions |
| Direct messages between accounts, with Carbons on the Waddle side | Both directions | Both directions | Both directions |
| Local user joins remote MUC, sends, reads MAM | Required | Required | Required |
| Remote user joins Waddle MUC, sends, reads MAM | Required | Required | Required |
| Disco#info, disco#items and ping across S2S | Required | Required | Required |
| Waddle client uses a Space hosted on the peer's PubSub service | Required | Required (stock PubSub) | Required (stock PubSub) |
| Peer account reads a Waddle Space with plain XEP-0060 | Required | Required | Required |
| Every child room joins by plain MUC JID | Required | Required | Required |

Movim and Slidge Spaces checks are recorded but do not block.

The negative matrix runs Waddle ↔ Waddle, and against each third party where
it can be driven. Unless stated, the expected result is refusal without local
effect and without disturbing unrelated traffic.

| Area | Cases |
| --- | --- |
| Certificates and trust | Wrong or expired certificates; a parent-domain certificate for a subdomain; serverAuth-only certificates, which must authenticate in both roles; a public-CA certificate for a domain configured with a private-CA profile, and a private-CA certificate for a public-profile domain, which must fail; an inbound stream without a pre-TLS `from`; an inbound direct-TLS connection attempt |
| Dialback | Dialback from a domain not on the dialback list; dialback after an active certificate failure; unsolicited or uncorrelated `db:result` and `db:verify`; extra sender or target domains on an EXTERNAL-authenticated or dialback stream; a peer verifying Waddle's dialback key over a non-TLS connection |
| Discovery (stub resolver) | An SRV target or A/AAAA fallback address in each blocked class, including IPv4-mapped and NAT64 forms; a DNS answer that changes between check and connect; SERVFAIL and timeout, which must not fall back to A/AAAA; a "." target, which forbids fallback; target and address caps; a certificate naming only the SRV target |
| Routing | Spoofed `from`; forged and replayed IQ results; an IQ reply arriving on a different replica from the request; relay through an unserved domain; remote traffic to a local account, refused per D7 before W6c and handled per RFC 6121 after it; no error in reply to an error stanza |
| Admission | A denylisted domain with an open stream and queued work; allowlist removal and a switch to `closed` with open streams and queued work; many peers exhausting slots in `open` mode, with global caps, eviction and backoff holding |
| Authority | A remote principal with a stored privileged grant, on `pkix` and on dialback-only streams, refused privileged effects; privileged grant issuance to a remote JID; grant and revoke races across two replicas; stale repair; scoped administration, with one community's owner targeting another; denied private discovery and MAM; outcast precedence on every channel action |
| Identity | Identically named rooms on both sides; identity collisions; malicious localparts and colliding Unicode IDs (case, sharp-s, trailing dot, A-label and U-label); a reassigned JID: a Waddle home refuses reuse, while a third-party account deleted and re-registered keeps only member-level grants until they are revoked, never privileged ones |
| Resources and restart | Oversized and deeply nested stanzas; per-peer queue exhaustion; every restart combination |

The product journeys in
[`docs/product/critical-journeys.json`](../product/critical-journeys.json) keep
their own gate. This verification contributes evidence but does not satisfy
the Snikket peer, file exchange in remote rooms, message moderation, the
journeys' ordered and deduplicated delivery wording or non-web clients.

## Risks

- **Deferred execution.** D9 defers test execution to verification. The TLS
  verifier (W4b) and the grant fence (W5b, W5d) are the code most likely to
  compile and still be wrong, so their focused tests run on completion; other
  defects, such as in the identity profile (W5i) or the effect boundary (W5e),
  surface only at verification and can rework several later slices.
- **Long-lived stack.** More than twenty draft PRs stay open until
  verification. Keep slices small and rebase the integration branch once per
  merge round.
- **#2046 on the critical path.** If its role and transaction cutover slips,
  everything from W5b on waits. W5e also waits for the transport chain.
- **Open federation versus spoofing.** Without DNSSEC, dialback lets a network
  attacker impersonate a domain. The `allowlist` default holds until the D1
  evidence passes, and dialback-only streams never carry privileged authority;
  do not relax either to meet the open-federation goal early.
- **No remote administration.** Under D8 a community cannot be administered by
  an account homed elsewhere, including another Waddle deployment.
- **Interoperability of strict choices.** Requiring a pre-TLS `from`, refusing
  multiplexed dialback domains and refusing A/AAAA fallback after a failed SRV
  lookup are stricter than RFC 6120 and XEP-0220 require; the harness records
  whether Prosody and ejabberd connect under them.
- **Third-party Spaces.** No Prosody or ejabberd XEP-0503 module was found, so
  third-party Spaces acceptance rests on plain PubSub and the MUC fallback
  (ADR-0020 D4); W7a repeats that survey.
- **Lexical inventory.** The #2062 writer inventory is a token scan at an older
  baseline, not a call graph. W5c starts with a fresh sweep and manual review;
  an unfenced writer found later reopens W5d.
