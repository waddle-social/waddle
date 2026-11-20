# Issue 2047 — plan v3: open XMPP and Spaces federation

This is a delivery plan; it establishes no implementation, interoperability or
deployment readiness. Architecture decisions D1–D6 are recorded in
[ADR-0020](../../server/docs/adrs/0020-independent-community-federation.md),
and its implementation gates bind every slice below. Plan v3 replaces plan v2.

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
- Hosted CI is disabled; evidence is recorded by hand in `docs/evidence/2047/`.

## Phases

| Phase | Scope | Slices |
| --- | --- | --- |
| P0 | Architecture decisions, review gates and pinned sources | W0 |
| P1 | Peer discovery, authentication and S2S transport | W4a–W4d |
| P2 | Routing, authorization identity and host grant authority | W1, W2, W5a–W5d |
| P3 | MUC and client interoperability | W3, W6a, W6b, W8 |
| P4 | Spaces interoperability | W7a, W7b, W8 |
| P5 | Two-deployment and third-party interoperability harness | W10 |

Verification and capability advertisement (W11) follow the slices. AT Protocol
home login moves to its own issue (D6) and depends on W1; it has no slice here.

Earlier documents use plan v2 phase names: F0 is P0 and the design gates; F1 is
W1, W2 and W3; F2 is W4a–W4d and W5a; F3 is W5b–W5d; F4 is W8 with W6 and W7;
F5 is W10 and the verification phase.

## Slices

Each slice is one branch and one draft PR, stacked in dependency order, with
Conventional Commits using one scope. Each slice ships its tests compiled but
not executed.

| Slice | Phase | Scope | Main files | Depends on |
| --- | --- | --- | --- | --- |
| W0 | P0 | Amend ADR-0020 for D1–D6; plan v3; reconcile the design ledgers; pin RFC and XEP sources | `server/docs/adrs/0020-…`, `docs/planning/federation-2047.md`, `server/docs/planning/*-2047.md` | Accepted decisions |
| W1 | P2 | Production authorization ID codec: promote the `v2_` codec into `permissions/`, typed subject and object IDs, writer and reader inventory, empty-namespace proof or audited migration, fail closed on legacy IDs | `permissions/spicedb.rs`, `experiments/authz-id-codec` | — |
| W2 | P2 | Destination classifier before namespace dispatch: missing, local bare, local full, each served component, remote, invalid; generalize the foreign-request rejections; IQ result and error routing; no error loops | `iq/dispatch.rs`, `route_to_connection/implementation.rs`, `server/config.rs` | — |
| W3 | P3 | Client qualified identity: room is the bare room JID, Space is service JID plus node, message IDs are scoped by conversation and authority; caches, drafts, unread, reactions and routes; no cross-community UI yet | `chat/src`, `waddle-client-session`, `waddle-xmpp-client` | — |
| W4a | P1 | `waddle-xmpp::s2s`: bounded streaming XML codec for `jabber:server`, typed stream headers, negotiation state machine, typed stream errors (`improper-addressing`, `host-unknown`, `invalid-from`) | new `crates/waddle-xmpp/src/s2s/` | — |
| W4b | P1 | Verifier per the verifier design as amended by D2: rustls client and server verifier traits; DNS-ID, SRV-ID and XmppAddr checks; chain bounds; strict per-peer private-CA profile with hard CRL checks; public-WebPKI profile for `open` mode accepting serverAuth-only EKU in both roles | new `crates/waddle-server/src/s2s/verify/` | W0 |
| W4c | P1 | Connection manager: 5269 and direct-TLS listener through `LISTEN_FDS` name `s2s`, outbound dialer, STARTTLS, SASL EXTERNAL, `VerifiedPeerStream`, per-peer limits, deadlines, shutdown join | `server/lifecycle.rs`, `main.rs`, new `s2s/conn/`, `s2s/config.rs` | W4a, W4b, D1 |
| W4d | P1 | Discovery under D3, dialback under D2, admission modes under D1 | new `s2s/discovery.rs`, `s2s/dialback.rs` | W4c |
| W5a | P2 | S2S routing: typed `RemotePrincipal` carrying auth strength; ingress from-domain validated against the stream; bounded per-peer egress queues with backpressure; IQ correlation with at-most-once completion and expiry; W2's remote branch connected to egress | new `s2s/route/`, `routes/interpret/` | W1, W2, W4c |
| W5b | P2 | Grant authority part 1 (D5): PostgreSQL canonical store, durable scope and generation, mutation linearization, SQL admission inside the acceptance transaction, fenced SpiceDB projection, recovery of unknown writes; channel schema `read`, `send_message` and `view` subtract `outcast` | `permissions/`, new migrations | W0, W1, #2046 role and transaction cutover |
| W5c | P2 | Grant authority part 2: fence every writer in the #2062 inventory, starting from a fresh sweep; retire the outcast-overwriting owner reseed, the latent `set_room_affiliation` path and the graph-first MUC admin write; one admission check on join, send, MAM, fanout and queued work | `server/bootstrap_membership.rs`, `spaces_pubsub_seed.rs`, PubSub and MUC writers | W5b |
| W5d | P2 | Revocation: acknowledged fence, non-reuse of retired JIDs on Waddle homes, re-verification after a reported remote reassignment; the #2056 Space-fanout negative control becomes a passing fence test | `permissions/`, Space fanout | W5c |
| W6a | P3 | Inbound MUC: remote occupants join local rooms (XEP-0045), presence and message fanout over S2S, host-authorized MAM (XEP-0313), disco#info and disco#items for remote askers | MUC handlers, XEP-0045 and XEP-0313 paths | W5a, W5c |
| W6b | P3 | Outbound MUC: local users join remote rooms through C2S-to-S2S presence, self-ping (XEP-0410), occupant cleanup on S2S loss | `route_to_connection`, presence routing | W5a |
| W7a | P4 | Spaces over S2S: XEP-0503 0.3.0 PubSub on remote service JIDs; subscription is not a room grant; no invented inheritance; avatar and banner fetching off; module header corrected to 0.3.0 | `xep0503.rs`, PubSub service | W5c, W6a |
| W7b | P4 | MUC fallback: clients without Spaces reach every child room by plain MUC JID, and disco items list them | Spaces and MUC disco | W7a |
| W8 | P3/P4 | Web pilot: remote community discovery, join, XEP-0402 bookmarks for remote rooms, outage versus denial versus deletion states | `chat/src` | W3, W6b, W7a |
| W10 | P5 | Harness: two native Waddle processes with separate PostgreSQL, SpiceDB and secrets; pinned Prosody and ejabberd 25.08 or later (`s2s_use_starttls: required`) through Nix; `.test` CA; serverAuth-only certificate cases | new `server/experiments/federation-interop/`, `flake.nix` | W4c; written during implementation, run in verification |
| W11 | — | Capability advertisement: add only what verification demonstrated, per client and per release | `capabilities.toml`, disco | Verification |

The critical path runs through the grant fence: W0, W5b, W5c, W5d, W6a, W7a.
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
- Review the pinned source in `xeps/` (xsf/xeps
  `4132c531487fe9450ae90b905b8df950d0d478b2`) before changing wire behavior,
  and record the commit in the PR.
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

## Gates during implementation

No test runs until every slice is merged into `integration/2047`; tests are
still compiled, because a test that does not build is a defect found early.
Run each command on its own line from the repository root inside the Nix
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

Not allowed before verification: `cargo nextest run`, `cargo test`,
`bun test`, any `server/experiments/*/run-native.sh`, the interoperability
harness, and `cargo check --lib --tests` on `waddle-server`.

Each slice lists its unrun tests (file, test names and prerequisites such as
PostgreSQL, SpiceDB, Prosody, ejabberd or two processes) in
`docs/evidence/2047/unrun-tests.md`, which is the verification checklist.
After each round of merges, the integration branch runs these gates once on the
combined tree and is rebased on main.

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
   codec and W4b verifier matrix, then the XEP suites for 0030, 0045, 0060,
   0199, 0280, 0313, 0402, 0410 and 0503.
3. **`waddle-server` library unit tests** (the 6–10 GB build).
4. **Native PostgreSQL and SpiceDB experiments:** grant-fence, room-child,
   parent-move and space-fanout. The #2056 negative control must now reject
   the stale delivery; the old behavior passing is a W5d regression.
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
   integrated head, repeated after any fix until it returns CLEAN. W11 then
   advertises only what steps 6–8 demonstrated, and the D1 default moves from
   `allowlist` to `open` only once its named evidence has passed.

| Flow | Waddle ↔ Waddle | Waddle ↔ Prosody | Waddle ↔ ejabberd |
| --- | --- | --- | --- |
| S2S with TLS and SASL EXTERNAL | Both directions | Both directions | Both directions |
| Dialback under the D2 policy | Refused unless policy allows | Both directions | Both directions |
| Local user joins remote MUC, sends, reads MAM | Required | Required | Required |
| Remote user joins Waddle MUC, sends, reads MAM | Required | Required | Required |
| Disco#info, disco#items and ping across S2S | Required | Required | Required |
| Waddle client uses a Space hosted on the peer's PubSub service | Required | Required (stock PubSub) | Required (stock PubSub) |
| Peer account reads a Waddle Space with plain XEP-0060 | Required | Required | Required |
| Every child room joins by plain MUC JID | Required | Required | Required |

Movim and Slidge Spaces checks are recorded but do not block.

The negative matrix covers Waddle ↔ Waddle, and each third party where it can
be driven: spoofed `from`, forged and replayed IQ results, relay through an
unserved domain, wrong or expired certificates, a parent-domain certificate
for a subdomain, serverAuth-only certificates (which must authenticate in both
roles), dialback from an unlisted domain or after a certificate failure, a
denylisted domain with an open stream and queued work, oversized and deeply
nested stanzas, per-peer queue exhaustion, grant and revoke races across two
replicas, identically named rooms on both sides, a reassigned JID, and every
restart combination.

## Risks

- **Deferred execution.** The TLS verifier (W4b) and the revocation fence (W5b,
  W5d) are the code most likely to compile and still be wrong, and W6–W8 build
  on them. A fence defect found at verification step 4 reworks several later
  slices.
- **Long-lived stack.** About twenty draft PRs stay open until verification.
  Keep slices small and rebase the integration branch once per merge round.
- **#2046 on the critical path.** If its role and transaction cutover slips,
  everything from W5b on waits.
- **Open federation versus spoofing.** Without DNSSEC, dialback lets a network
  attacker impersonate a domain. The `allowlist` default holds until the D1
  evidence passes; do not relax it to meet the open-federation goal early.
- **Third-party Spaces.** Prosody and ejabberd have no XEP-0503 module, so
  third-party Spaces acceptance rests on plain PubSub and the MUC fallback
  (ADR-0020 D4).
- **Lexical inventory.** The #2062 writer inventory is a token scan at an older
  baseline, not a call graph. W5c starts with a fresh sweep and manual review;
  an unfenced writer found later reopens W5d.
