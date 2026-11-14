# Issue 2047 — plan v3: open XMPP and Spaces federation

This is a delivery plan; it establishes no implementation, interoperability or
deployment readiness. Architecture decisions D1–D11 and gates G1–G24 are
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
- A remote account holds a privileged grant or moderates a room on a host
  only when its home is on the host's trusted-home list and its stream is
  PKIX-authenticated, and a grant that is not honored contributes nothing
  (D8). Two Waddle deployments that list each other can administer each
  other's communities that way; every other remote account holds only
  member-level grants.
- Remote senders reach only MUC, the Spaces PubSub service, service discovery
  and ping, and local accounts once account traffic lands (D10, D7).
- Presence and direct messages between accounts on different domains are in
  scope (D7).
- S2S is off by default, and nothing is advertised until verification
  demonstrates it. Enabling S2S in a deployment is a separate decision with a
  named window and runbook.
- No slice reaches main before verification and the close-out review; then
  every slice reaches main together under one rollout runbook, once the owner
  has given an explicit go-ahead with a named window (D11, G24).
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
- The GitHub workflow sources are restored (#2395), and hosted CI runs on every
  push, including pushes to draft pull requests. Main deploys automatically.

## Phases

| Phase | Scope | Slices |
| --- | --- | --- |
| P0 | Architecture decisions, gates and pinned sources | W0 |
| P1 | Peer discovery, authentication and S2S transport | W4a–W4d |
| P2 | Routing, authorization identity and host grant authority | W1, W2, W5i, W5o, W5a–W5e, W5r |
| P3 | MUC, account traffic and client interoperability | W3, W6a–W6c, W8 |
| P4 | Spaces interoperability | W7a, W7b, W8 |
| P5 | Two-deployment and third-party interoperability harness | W10 |

W13, the rollout runbook, is written alongside the slices it covers.
Verification follows the slices; capability advertisement (W11) and the D1
default change (W12) follow verification, and every slice then reaches main
together under W13 (D11). AT Protocol home login moves to its own issue (D6)
and depends on W1; it has no slice here.

Earlier documents use plan v2 phase names: F0 is P0 and the design gates; F1 is
W1, W2 and W3; F2 is W4a–W4d and W5a; F3 is W5i, W5o, W5b–W5e and W5r; F4 is W8
with W6 and W7; F5 is W10 and the verification phase.

## Slices

Each slice is one branch, stacked in dependency order, with Conventional
Commits using one scope, and merges into the local `integration/2047` branch.
Each slice ships its tests compiled but not executed, except where D9 says
otherwise.

| Slice | Phase | Scope | Main files | Depends on |
| --- | --- | --- | --- | --- |
| W0 | P0 | ADR-0020 decisions D1–D11 and gates G1–G24; plan v3; reconcile the design ledgers, including the G1 and G3 decisions W4a has recorded in the handshake design, which are reviewed before W4a merges; pin RFC and XEP sources | `server/docs/adrs/0020-…`, `docs/planning/federation-2047.md`, `server/docs/planning/*-2047.md` | Accepted decisions |
| W1 | P2 | Production authorization ID codec (G9): promote the `v2_` codec into `permissions/`, typed subject and object IDs, writer and reader inventory, empty-namespace proof or audited migration, fail closed on legacy IDs. Its rollout part of G24 is the cutover runbook `docs/operations/authz-codec-cutover.md`, which requires an empty SpiceDB store | `permissions/spicedb.rs`, `experiments/authz-id-codec`, `docs/operations/` | — |
| W2 | P2 | Destination classifier before namespace dispatch: missing, local bare, local full, each served component, remote, invalid; generalize the foreign-request rejections; IQ result and error routing; no error loops. Its rollout part of G24: mixed-version replicas, where old and new binaries classify the same destination differently during the rollout | `iq/dispatch.rs`, `route_to_connection/implementation.rs`, `server/config.rs` | — |
| W3 | P3 | Client qualified identity: room is the bare room JID, Space is service JID plus node, message IDs are scoped by conversation and authority; caches, drafts, unread, reactions and routes; no cross-community UI yet. Its rollout part of G24: client state stored under the old unqualified keys and clients older than the deployed server | `chat/src`, `waddle-client-session`, `waddle-xmpp-client` | — |
| W4a | P1 | `waddle-xmpp::s2s`: bounded streaming XML codec for `jabber:server`, typed stream headers, negotiation state machine, typed stream errors. Decides G3 and its part of G1 (header and stream-error behavior, including the error for a missing pre-TLS `from`, and SASL conformance), recorded in the handshake design | new `crates/waddle-xmpp/src/s2s/` | — |
| W4b | P1 | Verifier per the verifier design as amended by D2: rustls client and server verifier traits; per-domain trust profiles, public WebPKI by default and private-CA with hard CRL checks where configured; serverAuth-only EKU accepted in both roles in every profile; DNS-ID, SRV-ID and XmppAddr checks against reference identities in W5i's canonical domain form; chain bounds. Decides G2 and its part of G1: errata disposition, the TLS version profile (RFC 9325 section 3.1.1 requires TLS 1.2 support), the XmppAddr and wildcard policy, the public root store and public-chain revocation. Its focused set runs on completion (D9) | new `crates/waddle-server/src/s2s/verify/` | W0, W5i |
| W4c | P1 | Connection manager: STARTTLS-only `s2s` listener through `LISTEN_FDS` (no inbound direct TLS) with a mandatory pre-TLS `from`; a pre-authentication decoder mode that refuses a first-level element outside the stream, TLS, SASL and dialback namespaces when its head closes, before its tree is built, and keeps the control-element ceiling until SASL succeeds; outbound dialer with STARTTLS only (XEP-0368 direct TLS moves to W4d), ending an attempt on `see-other-host`; stream header addresses parsed from the raw text into W5i's canonical form, U-labels emitted, a new authenticated stream for a pair replacing the old one with rate limiting, and the other decisions the owner took after the G4 review; SASL EXTERNAL; `VerifiedPeerStream` carrying authentication strength; D1 modes and lists and the D8 trusted-home list in W5i's canonical form, with exact allowlist, dialback and trusted-home entries and denylist entries covering subdomains, applying in every mode and overriding the other lists; a configuration that lists a domain the denylist covers as allowed, dialback or trusted fails to load; IP-address, numeric IPv4 and single-label domains refused as W5i's canonical form classifies them; snapshot or reviewed reload, and closing the streams of a domain no longer admitted; per-peer limits and global peer, socket and handshake caps with eviction and backoff (G7); deadlines; shutdown join. Merges only after the G4 design review and W4b's focused set pass | `server/lifecycle.rs`, `main.rs`, new `s2s/conn/`, `s2s/config.rs`, the pre-authentication mode of the `crates/waddle-xmpp/src/s2s/` decoder | W4a, W4b, W5i |
| W4d | P1 | Discovery under D3 (G6): outbound XEP-0368 direct TLS to `_xmpps-server` targets (SNI and ALPN `xmpp-server`), deferred from W4c; merged SRV ordering, "." targets, A/AAAA fallback only on a negative answer, blocked address classes including IPv4-mapped, NAT64, 6to4 and Teredo forms, checked on the address actually connected; target and address caps; lookups for the fully qualified name without a resolver search list; IP-address, numeric IPv4 and single-label domains, as W5i's canonical form classifies them, never resolved or dialled; configured endpoints; a stub-resolver suite. Dialback under D2 (G5): inbound behind its own list and TLS, outbound Initiating and Authoritative Server roles over TLS only, XEP-0185 keys, one domain pair per stream, rejection of unsolicited `db:result` and `db:verify`, the `dialback` strength tag, wire profile, active-failure classification and the residual risk of keys not bound to the TLS channel. The reviewed change lifting the loopback and `.test` restriction (G6) | new `s2s/discovery.rs`, `s2s/dialback.rs` | W4c |
| W5i | P2 | Identity profile, host-grant B6 beyond the codec (G10): choose the profile, including the canonical domain form (RFC 7622 preparation, trailing dot, U-label and A-label equivalence) that W4b, W4c, W4d, D1's lists and W5a use, and its classification of the domains D3 refuses: both RFC 7622 IP-address forms (bracketed `IP-literal` and `IPv4address`), other names a resolver or address parser accepts as an IPv4 address (`2130706433`, `0x7f.1`, `127.1`) and unqualified single-label names; enforce it on XML-decoded identity inputs before lossy JID preparation at C2S parsing, provisioning, authentication and directory lookup; canonical bare-JID account binding; authoritative reconciliation. Its rollout part of G24: existing accounts outside the profile and old binaries | `waddle-xmpp` frame parsing, `auth/directory.rs`, provisioning paths | W0, W1 |
| W5o | P2 | Outcast precedence as its own behavior change: every channel permission (`read`, `send_message`, `view`, `moderate`, `manage`, `delete`) and every message permission in that room, including the author's own `edit` and `delete`, subtracts `outcast`, while a moderator can still remove an outcast's messages through the moderator's own `moderate`; fenced schema transition in which startup compares a versioned schema and refuses to overwrite a different one (parts of G14 and G15). Its rollout part of G24: mixed-version replicas and old-binary retirement | `permissions/schema.rs`, `permissions/actor.rs`, `permissions/spicedb.rs` | W0, W1 |
| W5a | P2 | S2S routing: typed `RemotePrincipal` carrying domain and authentication strength; G10 enforcement at S2S ingress; ingress from-domain validated against the stream; bounded per-peer egress queues with backpressure; IQ correlation with at-most-once completion and expiry, including a reply that reaches another replica; W2's remote branch connected to egress for messages, presence and IQs; destinations refused that W5i's canonical form classifies as IP-address, numeric IPv4 or single-label domains; federated handoff contract (G8); D10 reachability, with an inventory of every local service and handler a stanza from an S2S stream can reach and refusal outside MUC, Spaces PubSub, disco and ping (G23), and server-domain disco#info, disco#items and ping answered for remote askers; D7's interim refusal in both directions. The inbound refusal is tested in its own suite against a local-account destination that no reviewed handler serves; the outbound refusal is reviewed, not run, because W6c replaces it and rewrites those cases before verification | new `s2s/route/`, `routes/interpret/` | W1, W2, W4c, W5i |
| W5b | P2 | Grant authority part 1, D5's first slice: PostgreSQL canonical store, durable scope and generation, mutation linearization, SQL admission inside the fresh-manifest acceptance transaction for one fresh isolated community, a managed members-only room and explicit affiliations, independent of SpiceDB availability; fenced SpiceDB projection for that relation family, with its marker protocol (part of G15); recovery of unknown writes; store capabilities and credential separation (G11); lock order (G12); privileged and moderator grants to remote JIDs refused at issuance unless the home is in the trusted-home set the issuance path is given (D8). Its focused set runs on completion (D9) | `permissions/`, new migrations, `ingress/commit/`, `ingress/commit_room.rs` | W0, W1, W5i, W5o, #2046 role and transaction cutover |
| W5c | P2 | Grant authority part 2: fresh writer sweep and fencing of every writer (G11), with bootstrap and repair refusing privileged and moderator grants to remote JIDs whose home is not in the trusted-home set they are given (D8); retire the outcast-overwriting owner reseed, the latent `set_room_affiliation` path and the graph-first MUC admin write; one admission check on joins, protected reads (MAM, disco), PubSub and stream-management fanout (G16), queued work and room administration, with the lock order extended to each surface (G12) | `server/bootstrap_membership.rs`, `spaces_pubsub_seed.rs`, PubSub and MUC writers | W5b |
| W5d | P2 | Revocation: acknowledged fence; non-reuse of retired JIDs on Waddle homes; the operator procedure after a reported third-party reassignment (D8); the #2056 Space-fanout negative control becomes a passing fence test. Its focused set runs on completion (D9) | `permissions/`, Space fanout | W5c |
| W5e | P2 | Effect-boundary admission for remote principals (G17): the admission witness binds the remote domain, its authentication strength, the current D1 admission decision (including a denylisted parent domain) and the D8 trusted-home status; work from a domain no longer admitted is rejected; privileged authority and room moderation are honored only for a trusted home on a `pkix` stream, and a grant that is not honored contributes nothing, including no member-level permission derived from it; host policy can require `pkix`; the trusted-home list from W4c's configuration feeds issuance, bootstrap, repair and admission | `permissions/`, admission witness | W5a, W5c |
| W5r | P2 | Restore incarnation, host-grant B7 (G13): an authority-incarnation transition that survives PostgreSQL rollback, invalidation of old credentials and work, verified reconstruction before admission. Its rollout part of G24 | `permissions/`, startup, new migration | W5b |
| W6a | P3 | Inbound MUC: remote occupants join local rooms (XEP-0045), presence and message fanout over S2S, host-authorized MAM (XEP-0313), disco#info, disco#items and ping for remote askers; the moderator role and room administration for remote occupants only as D8 allows | MUC handlers, XEP-0045 and XEP-0313 paths | W5a, W5d, W5e |
| W6b | P3 | Outbound MUC: local users join remote rooms through C2S-to-S2S presence; traffic from a joined remote room to the occupant's full JID is admitted, and a local account's messages and presence for a remote room it occupies or is joining are exempt from D7's outbound refusal; self-ping (XEP-0410); occupant cleanup on S2S loss | `route_to_connection`, presence routing | W5a |
| W6c | P3 | Account traffic across S2S (D7, G18): inventory of every local-account handler a remote sender can reach; RFC 6121 server rules for remote messages, presence, subscription requests and probes, and IQs to accounts; PubSub event notifications from remote services to local accounts; local accounts' presence, direct messages and subscriptions to remote contacts; Carbons (XEP-0280) for those messages; replaces W5a's refusal for reviewed handlers and rewrites W5a's outbound refusal cases for the traffic it enables | presence, message, roster and IQ handlers | W5a, W5c, W5d, W5e |
| W7a | P4 | Spaces over S2S: XEP-0503 0.3.0 PubSub on remote service JIDs; subscription is not a room grant; no invented inheritance; module header corrected to 0.3.0; the third-party XEP-0503 survey repeated before acceptance is fixed | `xep0503.rs`, PubSub service | W5c, W5d, W6a, W6c |
| W7b | P4 | MUC fallback: clients without Spaces reach every child room by plain MUC JID, and disco items list them | Spaces and MUC disco | W7a |
| W8 | P3/P4 | Web pilot: remote community discovery and join; XEP-0402 bookmarks for remote rooms; qualified joined-space preferences that are never authority; leave, unsubscribe, forget and revoke kept distinct; outage versus denial versus deletion states; no fetching of Space avatar or banner URLs (D4) | `chat/src` | W3, W6b, W7a |
| W10 | P5 | Harness: two native Waddle processes, `home-a.test` and `home-b.test`, with separate PostgreSQL, SpiceDB and secrets, each listing the other as a trusted home; the third-party moderation flows list the peer's domain as a trusted home, and the untrusted-home cases run with it unlisted; pinned Prosody (`prosody.test`) and ejabberd 25.08 or later (`ejabberd.test`, `s2s_use_starttls: required`) through Nix, with accounts for presence and direct-message flows, and ejabberd also clustered with `max_s2s_connections` above one, against Waddle's one stream per directional pair; the `.test` CA configured as each domain's private-CA profile, including serverAuth-only leaves; loopback configured endpoints; records whether each peer sends a pre-TLS `from` and ejabberd's effective STARTTLS setting | new `server/experiments/federation-interop/` (its own Cargo workspace), `flake.nix` | W4c; written during implementation, run in verification |
| W11 | — | Capability advertisement (G21): add only what verification demonstrated, per client and per release; runtime advertisement, such as disco features, only in a deployment where S2S is enabled; delta review | `capabilities.toml`, disco | Verification and close-out review |
| W12 | — | D1 default change from `allowlist` to `open` with a denylist, once G20's evidence has passed; delta review; #2047 does not close before it | `s2s/config.rs` | Verification and close-out review |
| W13 | — | Rollout runbook (D11, G24): one runbook for merging every slice into main together, in the window the owner names when giving the explicit go-ahead: order, S2S left off, old-binary retirement, W1's empty-store cutover, W2's mixed-version routing, W3's client-state handling, W5i's existing-account handling, W5o's schema transition, and the migrations and startup checks of W5b–W5d and W5r; written as those slices land and reviewed in the close-out. At close-out, W13 also refreshes the status text of ADR-0020 and the design documents from the evidence log | new `docs/operations/federation-2047-rollout.md`; status sections of ADR-0020 and the design documents | W1, W2, W3, W5i, W5o, W5b, W5c, W5d, W5r |

The dependency graph is acyclic. Two chains meet at W5e. The grant-fence
chain is W0 and W1, then W5i and W5o, then W5b, which also needs #2046's role
and transaction cutover, then W5c and W5d. The transport chain is W1 and W5i,
then W4b (beside W4a), W4c and W5a. W5e needs W5a and W5c; W6a and W6c need it
and W5d. After them, W7a, W7b and W8 finish P3 and P4. W5d is on the
critical path: W6a, W6c and W7a wait for its focused set, so its gate is not
vacuous. W1–W3 and W4a do not wait for #2046.

## Rules for every slice

- Follow [`server/AGENTS.md`](../../server/AGENTS.md) and
  [the Rust style conventions](../rust-style.md): no XML through `format!` or
  concatenation; typed stanzas, `jid` types and `xep::*` namespace constants;
  raw bytes only at the TLS and codec boundary.
- No `unwrap()` in first-party production code. Clippy runs with
  `-D warnings`; any new `#[allow]` is justified in the slice's report and
  its pull request.
- XEP behavior that is added or changed updates its dedicated suite in the same
  slice.
- Review the pinned sources before changing wire behavior and record the
  commit in the slice's report: RFCs from the RFC Editor, XSF sources at
  xsf/xeps `4132c531487fe9450ae90b905b8df950d0d478b2`. `xeps/` is an untracked
  local cache that `cuenv.lock` fills from a different xsf/xeps commit, so
  fetch the pinned commit before relying on it.
- A slice that owns a decision gate records the decision in the design
  document the [gate table](#gate-ownership) names; a slice that creates that
  document creates it in `server/docs/planning/`. Every slice gets an
  independent review of its diff, including those decisions, before it merges
  into the integration branch; an unresolved blocking finding pauses the merge.
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
- Hosted CI runs on every push, including pushes to draft pull requests. To
  keep that load off shared runners, slice branches and the integration branch
  are not pushed, and pull requests are not opened or updated, until the
  verification phase; step 11 below says when they are.

## Static checks during implementation

Under D9, no test runs until every slice is merged into `integration/2047`,
except the [focused sets](#focused-sets-run-at-slice-completion) of W4b, W5b
and W5d. Tests are still compiled, because a test that does not build is a
defect found early. Run each command on its own line from the repository root
inside the Nix development shell, for touched crates only:

```
cargo fmt --manifest-path server/Cargo.toml --all -- --check
cargo clippy --manifest-path server/Cargo.toml -p waddle-xmpp --all-targets --locked -- -D warnings
cargo clippy --manifest-path server/Cargo.toml -p waddle-server --lib --locked -- -D warnings
cargo check --manifest-path server/Cargo.toml -p waddle-server --features test-support --locked --test <each new or changed test file>
git diff --check
```

W3 also checks each client crate it touches, such as `waddle-client-session`
and `waddle-xmpp-client`, and both chat slices (W3 and W8) run the chat
checks:

```
cargo clippy --manifest-path server/Cargo.toml -p waddle-client-session --all-targets --locked -- -D warnings
cargo clippy --manifest-path server/Cargo.toml -p waddle-xmpp-client --all-targets --locked -- -D warnings
bun run --cwd chat lint
bunx --cwd chat astro check
```

W10's harness is its own Cargo workspace with its own lockfile, outside the
server workspace like the existing experiments, and runs:

```
cargo fmt --manifest-path server/experiments/federation-interop/Cargo.toml --all -- --check
cargo clippy --manifest-path server/experiments/federation-interop/Cargo.toml --workspace --all-targets --locked -- -D warnings
```

Documentation slices (W0 and W13) run `git diff --check` and a relative-link
and anchor check of the files they change.

Not allowed before verification, apart from D9's exception: `cargo nextest
run`, `cargo test`, `bun test`, any `server/experiments/*/run-native.sh`, the
interoperability harness, and `cargo check --lib --tests` on `waddle-server`.

Each slice lists its unrun tests (file, test names and prerequisites such as
PostgreSQL, SpiceDB, Prosody, ejabberd or two processes) in its report. When
verification starts, they will be collected into
`docs/evidence/2047/unrun-tests.md`, the verification checklist; that
directory will also hold the evidence log. After each round of merges, the
integration branch runs these checks once on the combined tree and is rebased
on main.

## Focused sets run at slice completion

D9 lets three focused sets run as soon as their slice completes. Each set
contains only cases that its own slice and the slices it depends on can meet;
a case that needs a later slice is that slice's evidence and runs in
verification. Every set runs again on the integrated tree in verification.

| Set | Cases | Prerequisites | Must pass before |
| --- | --- | --- | --- |
| W4b | The rows marked W4b in [the verifier design's matrix](../../server/docs/planning/s2s-verifier-2047.md#required-executable-negative-test-matrix): the verifier adapter, startup trust inventory and CRL construction, in-memory rustls handshakes in both roles, and the bounded verifier worker | Rust toolchain only: no listener, second process, PostgreSQL or SpiceDB | W4c merges |
| W5b | The rows marked W5b in [the host-grant design's acceptance matrix](../../server/docs/planning/host-grant-authority-2047.md#required-native-acceptance-matrix): SQL admission in the fresh-manifest transaction, its mutation and recovery paths, and its projection writes | PostgreSQL 17 and production SpiceDB, independent processes | W5c and W5r merge |
| W5d | The rows marked W5d in the same matrix: the acknowledged revocation fence across two replicas, including queued work at W5c's surfaces; the #2056 Space-fanout control passing; non-reuse of retired JIDs on a Waddle home | PostgreSQL 17, production SpiceDB, two processes | W6a, W6c and W7a merge |

## Gate ownership

The ADR defines each gate's kind and what it blocks. This table names its
owner, the design document that records its decisions, the point by which
those decisions must be made and reviewed, and where its evidence is
discharged. Documents named without a link do not exist yet; the owning slice
creates them. Step numbers refer to the verification phase below.

| Gate | Owner | Decisions recorded in | Decided and reviewed before | Evidence discharged at |
| --- | --- | --- | --- | --- |
| G1 Transport standards profile | W4a (headers, stream errors, SASL), W4b (errata, TLS versions, identity matrix), W5i (canonical domain form and IDNA) | [Handshake design](../../server/docs/planning/s2s-handshake-2047.md), [verifier design](../../server/docs/planning/s2s-verifier-2047.md), host-grant design (domain form) | Each owner's merge, and all of them before the G4 review | Steps 2 (W4a suites, W4b set, W5i identity suite, XEP-0178 suite), 6 and 7 |
| G2 Concrete verifier | W4b | Verifier design | W4b merge | W4b set on completion (D9); steps 2 and 6 |
| G3 Codec and restart buffers | W4a | Handshake design | W4a merge; the decisions are already recorded there and are reviewed before that merge | Step 2 |
| G4 Transport design review | Independent reviewer, on the recorded G1–G3 decisions | Handshake design | W4c merge | Review only |
| G5 Dialback profile, active failures and residual risk | W4d | `server/docs/planning/s2s-dialback-2047.md` | W4d merge; enabling inbound dialback | Steps 2 (XEP-0220 suite with XEP-0185 keys), 6 and 7 |
| G6 Discovery profile, addressing and the loopback and `.test` restriction | W4d, using W5i's domain classification | `server/docs/planning/s2s-discovery-2047.md` | W4d merge | Step 2 (XEP-0368 stub-resolver suite, including lookups without a resolver search list); step 6 for configured endpoints, refused domain forms and redirects |
| G7 Admission configuration and resource model | W4c | `server/docs/planning/s2s-connection-manager-2047.md` | W4c merge | Steps 6 and 8 |
| G8 Federated handoff contract | W5a | `server/docs/planning/s2s-routing-2047.md` | W5a merge | Steps 6, 7 and 8 |
| G9 Authorization ID codec | W1 | [Host-grant design](../../server/docs/planning/host-grant-authority-2047.md); W1's cutover runbook | W1 merge | Steps 2 and 4 |
| G10 Identity profile | W5i, applied at S2S ingress by W5a | Host-grant design | W5i merge; W5a merge | Steps 2, 4 and 6 |
| G11 Writer exclusion, store capabilities, credentials | W5b, W5c | Host-grant design | Each owner's merge | W5b set (D9); step 4; old binaries and credentials retired under G24 and G22 |
| G12 Lock order and atomic admission | W5b, extended by W5c | Host-grant design | Each owner's merge | W5b set (D9); step 4 |
| G13 Restore incarnation | W5r | Host-grant design | W5r merge | Step 4 |
| G14 Complete policy | W5b, W5c and W5e per surface; W5o for outcast precedence | Host-grant design | Each owner's merge | Steps 2 (W5o suite), 4 and 6 |
| G15 Projection and schema protocol | W5b (marker protocol for its projection writes), W5c (marker protocol for the writers it fences), W5o (schema transition), W5r (bounded reconstruction) | Host-grant design | Each owner's merge; any slice that would read the projection for admission | W5b set (D9); step 4 |
| G16 PubSub, DirectFrame and sink contracts | W5c | Host-grant design | W5c merge | W5d set with the #2056 control (D9); step 4 |
| G17 Effect-boundary admission for remote principals | W5e | Host-grant design | W5e merge | Steps 6 and 7 |
| G18 Remote traffic to local accounts | W6c | `server/docs/planning/s2s-account-traffic-2047.md` | W6c merge | Steps 2 (RFC 6121 account-traffic suite), 6 and 7 |
| G19 Release evidence and close-out review | Verification | Evidence log in `docs/evidence/2047/` | — | Steps 6, 7, 8 and 9 |
| G20 D1 default change | W12 | The negative matrix below marks its cases | — | Step 7 for the cases marked G20, steps 2 and 4 for the codec, steps 4 and 6 for host-grant admission, then W12's delta review in step 10 |
| G21 Advertisement | W11 | W11's change | — | Steps 6 to 9, then W11's delta review in step 10 |
| G22 Activation | Separate owner decision | Activation runbook | — | Its runbook, including a check of the public-WebPKI profile against a real public peer and the statement that listing a trusted home delegates privileged authority and room moderation to that home's operator and to whoever later controls the domain |
| G23 Remote reachability of local services | W5a | `server/docs/planning/s2s-routing-2047.md` | W5a merge; each later change that enables a service or operation for remote senders | Step 2 (W5a reachability suite), steps 6 and 7 |
| G24 Rollout of changes that alter behavior while S2S is off | W13, with rollout parts from W1, W2, W3, W5i, W5o, W5b–W5d and W5r; the owner's explicit go-ahead with a named window | `docs/operations/federation-2047-rollout.md` | W13 merge; reviewed again in step 9 | Step 4 (W5o mixed-version schema case, W5r restore schedule), step 9, then the owner's go-ahead and step 11 |

Gate status, the evidence for it and every skipped prerequisite are recorded
in the evidence log in `docs/evidence/2047/`. The design documents and
ADR-0020 keep their gates open until then; at close-out, W13 refreshes their
status text from that log.

## Verification phase

Verification runs once, on `integration/2047`, cheapest step first, so a
failure points at one slice before the slow suites start. It needs a host with
at least 16 GB of RAM, PostgreSQL 17, SpiceDB and Nix. Failures are grouped by
slice and fixed on that slice's branch; after a fix, rerun the failing step and
every later step, then merge again. Skipped prerequisites are recorded as
unverified, never as passed.

1. **Static, whole tree:** workspace Clippy with `--all-targets -D warnings`
   (including the `waddle-server` library test crate), `cargo fmt --check`,
   the W10 workspace checks, chat lint and `astro check`.
2. **Focused suites in dependency order:** the W1 codec, W2 classifier, W4a
   suites (`s2s_codec` and `xep0178_s2s_negotiation`), the W4b set, the W5i
   identity profile, the W5o outcast and schema suites, and the W5a
   reachability (D10) and inbound D7 refusal suites; the transport XEP suites
   for 0178, 0220 (with XEP-0185 keys) and 0368 (discovery with a stub
   resolver); then the XEP suites for 0030, 0045, 0060, 0115, 0199, 0280,
   0313, 0359, 0402, 0410 and 0503, and the RFC 6121 account-traffic suite.
3. **`waddle-server` library unit tests** (the 6–10 GB build).
4. **Native PostgreSQL and SpiceDB experiments:** grant-fence, room-child,
   parent-move and space-fanout; the W5b and W5d sets on the integrated tree;
   the host-grant matrix rows owned by W5c and W5r, including the W5r restore
   schedule; and the W5o mixed-version schema case. The #2056 negative control
   must now reject the stale delivery; the old behavior passing is a W5d
   regression.
5. **Full workspace:** `cargo nextest run --workspace --all-targets --locked
   --profile ci`, `cargo test --doc --workspace --all-features`, and
   `bun test` in `chat`.
6. **Two independent Waddles** (W10; `home-a.test` and `home-b.test` with
   separate databases, SpiceDB and secrets, real WSS login and S2S): first
   with S2S disabled, showing no `s2s` listener, dial, credential read or
   advertisement; then the positive flows; then the negative matrix. A Space
   whose avatar and banner items point at a harness URL causes no fetch from
   the server or the web pilot (D4).
7. **Third-party interoperability** with the pinned Prosody and ejabberd, in
   both directions (table below), and the negative cases marked G20 with each
   of them as the peer.
8. **Failure and restart:** each side restarted while the other stays up;
   partition; a remote outage shown as distinct from denial; local traffic
   shown to keep flowing.
9. **Close-out:** an independent, read-only adversarial review of the
   integrated head, including W13's runbook, repeated after any fix until it
   returns CLEAN.
10. **After close-out:** W11 advertises only what steps 6–8 demonstrated, and
    W12 moves the D1 default from `allowlist` to `open` once G20's evidence
    has passed. Each gets an independent delta review of its own change before
    it merges into the integration branch. Steps 1, 2 and 5 then run again on
    the integration branch with W11 and W12 merged.
11. **Rollout:** once the owner has given an explicit go-ahead with a named
    window (G24), the branches are pushed, hosted CI runs on them and its
    results are recorded in the evidence log, and every slice, W11 and W12
    included, merges into main together under W13's runbook (D11). The merge
    proceeds only when the hosted checks pass at the exact head being merged.
    S2S stays off; activation is G22.

| Flow | Waddle ↔ Waddle | Waddle ↔ Prosody | Waddle ↔ ejabberd |
| --- | --- | --- | --- |
| S2S with TLS and SASL EXTERNAL, at each TLS version the G1 profile allows | Both directions | Both directions | Both directions |
| Dialback under the D2 policy | Inbound only for a listed domain; outbound both roles | Inbound only for a listed domain; outbound both roles | Inbound only for a listed domain; outbound both roles |
| Presence subscription and presence between accounts | Both directions | Both directions | Both directions |
| Direct messages between accounts, with Carbons on the Waddle side | Both directions | Both directions | Both directions |
| Local user joins remote MUC, sends, reads host-authorized history with paging | Required | Required | Required |
| Remote user joins Waddle MUC, sends, reads host-authorized history with paging | Required | Required | Required |
| An account with two resources in a remote room: self-presence, room traffic and its own messages on both | Required | Required | Required |
| A fresh device restores remote rooms and Spaces from XEP-0402 bookmarks and joined-space preferences | Required | Required | Required |
| Room moderation by a remote moderator whose home is a trusted home, over a `pkix` stream | Required | Required | Required |
| Administration of a community by an account homed on a trusted Waddle home, over a `pkix` stream | Required | — | — |
| Disco#info, disco#items and ping across S2S | Required | Required | Required |
| Waddle client uses a Space hosted on the peer's PubSub service | Required | Required (stock PubSub) | Required (stock PubSub) |
| Peer account reads a Waddle Space with plain XEP-0060 | Required | Required | Required |
| Every child room joins by plain MUC JID | Required | Required | Required |

Each flow that the web pilot supports is driven through it, and its
acceptance is recorded per flow; other clients need their own evidence. Movim
and Slidge Spaces checks are recorded but do not block.

The negative matrix runs Waddle ↔ Waddle. The cases in the last column are
G20's: they also run with the pinned Prosody and, separately, the pinned
ejabberd as the peer that drives them, and the harness records how each was
driven. Unless stated, the expected result is refusal without local effect and
without disturbing unrelated traffic.

| Area | Cases | Also with Prosody and ejabberd as the peer (G20) |
| --- | --- | --- |
| Certificates and trust | Wrong or expired certificates; a parent-domain certificate for a subdomain; serverAuth-only certificates, which must authenticate in both roles; a public-CA certificate for a domain configured with a private-CA profile, and a private-CA certificate for a public-profile domain, which must fail; a TLS version outside the G1 profile; an inbound stream without a pre-TLS `from`; an inbound direct-TLS connection attempt | Wrong or expired certificates; a parent-domain certificate for a subdomain |
| Dialback | Dialback from a domain not on the dialback list; dialback after an active certificate failure; unsolicited or uncorrelated `db:result` and `db:verify`; extra sender or target domains on an EXTERNAL-authenticated or dialback stream; a peer verifying Waddle's dialback key over a non-TLS connection | — |
| Discovery (stub resolver) | An SRV target or A/AAAA fallback address in each blocked class, including IPv4-mapped, NAT64, 6to4 and Teredo forms; a DNS answer that changes between check and connect; SERVFAIL and timeout, which must not fall back to A/AAAA; a "." target, which forbids fallback; target and address caps; a certificate naming only the SRV target | — |
| Addressing | A remote domain in each refused form (a bracketed `IP-literal`, an `IPv4address`, the numeric, hexadecimal and abbreviated IPv4 forms `2130706433`, `0x7f.1` and `127.1`, and an unqualified single-label name) in an inbound stream header, as an outbound destination and in a stanza address; a `see-other-host` stream error from a peer before and after TLS, which must not be followed | — |
| Routing | Spoofed `from`; forged and replayed IQ results; an IQ reply arriving on a different replica from the request; relay through an unserved domain; no error in reply to an error stanza; remote traffic to a local-account handler outside W6c's reviewed set, refused as if the account did not exist | Spoofed `from`; forged and replayed IQ results; relay through an unserved domain |
| Local services (D10) | A remote IQ request or message to HTTP upload, push, a community or extension service, or any other server-domain request outside the enabled set, which receives `service-unavailable`; remote presence to such a service, which is ignored | — |
| Admission | A denylisted domain with an open stream and queued work; a subdomain of a denylisted domain; a denylisted domain presented with a trailing dot, in its other label form (U-label or A-label) and in different case, all still denied; a denylisted domain in each admission mode; a configuration that lists a denylisted domain, or a subdomain of one, as allowed, dialback or trusted, which fails to load; a subdomain of an allowlisted, dialback-listed or trusted-home domain, which those entries do not cover; allowlist removal and a switch to `closed` with open streams and queued work; many peers exhausting slots in `open` mode, with global caps, eviction and backoff holding | — |
| Authority | A privileged grant held by an account of a trusted home, honored on `pkix` and refused on a dialback-only stream; privileged issuance, bootstrap and repair for a JID of a home not on the trusted-home list, refused, and such a grant stored outside the canonical path, ignored; a home removed from the trusted-home list, whose stored privileged grants are then ignored; a former admin of a home removed from the trusted-home list, who cannot moderate or read through that grant; room moderation by a remote moderator of a trusted home, honored on `pkix` and refused on a dialback-only stream, and by a remote moderator of a home not on the list, refused on `pkix`; a message author editing and deleting their own message, which the moderation rules do not restrict; grant and revoke races across two replicas; stale repair; scoped administration, with one community's owner targeting another; denied private discovery and MAM; outcast precedence on every channel and message action, including an outcast author's own edit and delete, while a moderator can still remove the outcast's messages | — |
| Identity | Identically named rooms on both sides; identity collisions; malicious localparts and colliding Unicode IDs (case, sharp-s, trailing dot, A-label and U-label); a reassigned JID: a Waddle home refuses reuse, while an account of a home not on the trusted-home list, deleted and re-registered, keeps only member-level grants until they are revoked | — |
| Resources and restart | Oversized and deeply nested stanzas; an unauthenticated peer sending a large first-level element, refused when its head closes; per-peer queue exhaustion; every restart combination | Oversized and deeply nested stanzas; per-peer queue exhaustion |

The product journeys in
[`docs/product/critical-journeys.json`](../product/critical-journeys.json) keep
their own gate. This verification contributes evidence but does not satisfy
the Snikket peer, file exchange in remote rooms, message moderation, the
journeys' ordered and deduplicated delivery wording or non-web clients.

## Risks

- **Deferred execution.** D9 defers test execution to verification. The TLS
  verifier (W4b) and the grant fence (W5b, W5d) are the code most likely to
  compile and still be wrong, so their focused sets run on completion; other
  defects, such as in the identity profile (W5i) or the effect boundary (W5e),
  surface only at verification and can rework several later slices.
- **Long-lived stack.** More than twenty slices stay local and unmerged in main
  until verification. Keep slices small and rebase the integration branch once
  per merge round.
- **#2046 on the critical path.** If its role and transaction cutover slips,
  everything from W5b on waits.
- **Longer transport chain.** W4b waits for W5i's canonical domain form, which
  waits for W1, so the transport chain cannot start before W1 and W5i.
- **One rollout.** Main deploys automatically, and D11 merges every slice at
  once. W1's cutover needs an empty SpiceDB store, and the identity profile,
  outcast precedence and grant authority change behavior with S2S off, so the
  rollout is a planned window under W13's runbook, not a routine merge.
- **Open federation versus spoofing.** A dialback key is not bound to the TLS
  channel that carries it, so a party in a man-in-the-middle position against
  a peer that does not validate Waddle's certificate can relay one. Dialback
  is off by default, the `allowlist` default holds until G20 passes, and
  dialback-only streams never carry privileged authority or room moderation;
  do not relax any of these to meet the open-federation goal early.
- **Remote authority rests on operator trust.** Listing a trusted home
  delegates privileged authority and room moderation to its operator and to
  whoever later controls the domain: a trusted home that reassigns a JID can
  pass both to a new holder, and a reassigned JID at any other home can
  inherit member-level grants. Waddle detects neither; revocation after a
  report is the remedy (D8).
- **Interoperability of strict choices.** Requiring a pre-TLS `from`, refusing
  multiplexed dialback domains, refusing A/AAAA fallback after a failed SRV
  lookup, refusing IP-address and single-label domains and never following
  `see-other-host` are
  stricter than RFC 6120 and XEP-0220 require; the harness records whether
  Prosody and ejabberd connect under them.
- **Third-party Spaces.** No Prosody or ejabberd XEP-0503 module was found, so
  third-party Spaces acceptance rests on plain PubSub and the MUC fallback
  (ADR-0020 D4); W7a repeats that survey.
- **Lexical inventory.** The #2062 writer inventory is a token scan at an older
  baseline, not a call graph. W5c starts with a fresh sweep and manual review;
  an unfenced writer found later reopens W5d.
