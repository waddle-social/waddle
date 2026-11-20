# ADR-0020: Open XMPP and Spaces federation

## Status

Accepted on 2026-11-13 for decisions D1–D6 below. They replace the restricted
two-Waddle allowlist pilot this ADR previously proposed: the target is now
open, standards-compatible XMPP federation with independently operated
servers, including Prosody and ejabberd, and XEP-0503 Spaces.

Accepting these decisions selects designs; it is not evidence that any of them
works. None of the [implementation gates](#implementation-gates) below or in
the linked designs has passed. Nothing in this ADR authorizes enabling S2S,
advertising federation or any production activation; activation remains a
separate explicit decision with a named window and runbook. The delivery
sequence is in [the issue 2047 plan](../../../docs/planning/federation-2047.md).

## Context

A user signed in to a Waddle home deployment should be able to use the same
home account and client session in communities hosted by other Waddle
deployments and by independently operated XMPP servers. Users homed on other
XMPP implementations should be able to join suitably configured Waddle-hosted
rooms and, where their software supports it, navigate Waddle Spaces. A
compatible peer needs no Waddle software, AT Protocol account, shared database
or prior bilateral registration; each operator keeps its own domain, abuse and
security policy. The goal is interoperability across compatible
implementations and their negotiated features. It is not a claim that every
server implements every XEP, and insecure or obsolete configurations need not
be accepted. Two independent Waddle deployments are an early milestone, not
the product boundary.

The home server authenticates the account and its bound resources. The
resource host decides access to its own communities and rooms. Each deployment
has its own persistence, secrets and administrative authority.

At main `f77c03814439cf52f859653bc78ad74b5ec61bac`, the deployable server
exposes WebSocket C2S only and has no S2S transport: startup acquires only the
`http` listener. Foreign ping, Carbons control, discovery and legacy session
requests are rejected locally (#2049, #2054, #2063, #2064), and local full-JID
IQ results and errors are forwarded (#2070). There is no general destination
classifier, and none of these paths is evidence of remote delivery. The cluster
relay operates within one deployment; it is not a federation trust boundary.
Federation is not advertised as supported in `server/capabilities.toml`.

Production permission actors require SpiceDB; the local tuple backend is a test
backend. This supersedes ADR-0009's statement that the implementation uses no
external authorization service, without changing its ReBAC model. Production
SpiceDB reads are fully consistent, but that does not serialize an authorization
check against a later effect or atomically coordinate PubSub storage, actors and
PostgreSQL. The existing SpiceDB ID substitution is not injective for all valid
JIDs. Both issues must be resolved before remote grants can be used.

## Decisions

| ID | Decision |
| --- | --- |
| D1 | Peer admission modes `closed`, `allowlist` and `open` with a denylist. S2S is off by default; `allowlist` is the default mode until named evidence passes. |
| D2 | TLS always. PKIX and SASL EXTERNAL are primary and accept serverAuth-only certificates in both roles. Inbound dialback is restricted; outbound dialback roles are implemented. |
| D3 | SRV discovery merges `_xmpps-server` and `_xmpp-server`. The source domain is the only reference identity; DNS never authorizes. |
| D4 | XEP-0503 is pinned at version 0.3.0; avatar and banner fetching is off. |
| D5 | PostgreSQL is the canonical grant authority and SpiceDB a fenced projection, with conditions. The first slice evaluates admission in SQL. |
| D6 | AT Protocol login moves to its own issue and ADR. The federated subject is a stable home bare JID derived from the DID, never the handle. |

## Decision: transport and addressing

- Use native XMPP S2S (`jabber:server` streams, RFC 6120) between Waddle and
  any compatible peer admitted under D1. Continue to use WebSocket for C2S; do
  not use cluster relay, a shared database, or a separate HTTP login or
  membership API as the federation protocol.
- Protect every S2S stream with TLS before dialback, SASL or any stanza:
  required STARTTLS, or direct TLS on `_xmpps-server` endpoints (XEP-0368).
  No plaintext or unauthenticated fallback is permitted; a TLS failure never
  leads to a plaintext retry.
- Streams are directional; XEP-0288 bidirectional streams are outside this
  profile. Every source and target domain pair used on a stream is
  authenticated on its own. Authenticating one domain on a stream, or a parent
  domain, does not authorize another domain, subdomain or component.
- [The handshake design](../planning/s2s-handshake-2047.md) and
  [the verifier design](../planning/s2s-verifier-2047.md) specify which
  component terminates TLS, verifies the peer and authorizes SASL, and how
  certificate rotation and connection resumption are revalidated. Both remain
  implementation gates.
- Validate the inbound stanza's originating domain against the authenticated
  stream and reject unowned recipients and unauthorized relay. A verified
  remote account has a typed remote principal, not a local `Session` or account.
  The canonical bare account JID is the grant and bootstrap subject; the bound
  full JID carries resource and routing provenance. Domain trust does not grant
  access to any community.
- Apply RFC 6120's S2S address validation: missing or invalid addressing closes
  the stream with `improper-addressing`, an unserved recipient domain with
  `host-unknown` or `host-gone`, and an unauthorized sender domain with
  `invalid-from`. C2S missing-destination rules must not be applied
  indiscriminately to S2S streams.
- Classify missing, local user, local service, remote and invalid destinations
  before local namespace dispatch. Preserve local bare/full-JID semantics and
  route IQ get, set, result and error explicitly. Correlation checks ID,
  allowed sender and recipient, request context, type, payload and expiry, and
  completes a pending request at most once. Distinguish transit IQ replies from
  requests locally originated by the server; accept legitimate protocol errors
  without satisfying unrelated requests. Return the appropriate bounded stream
  or stanza errors without error loops; do not fabricate success.
- Bound connections, stanza size and parsing, queues, retries, timeouts and
  per-peer work. A partition or malicious peer must not indefinitely block
  independent local traffic.

## Decision: peer admission (D1)

- S2S is off by default; enabling it is explicit operator configuration.
- An enabled deployment runs in one admission mode. `closed` admits no remote
  domain. `allowlist` admits only operator-listed domains. `open` admits any
  domain that authenticates under D2, except operator-denylisted domains.
- `allowlist` is the default mode until all of these pass: the Prosody and
  ejabberd negative matrix, fail-closed host-grant admission (phase P2), and
  the production authorization ID codec (W1 in the plan). The default then
  becomes `open` with a denylist; #2047 does not close before that change.
- Domain admission grants transport only. An admitted domain may hold streams
  and exchange stanzas under the routing rules above; it gains no community,
  room, PubSub or administrative access. Host policy decides each of those.
- The denylist is enforced when a stream opens and again at the
  protected-effect admission boundary, so a denylist change also applies to
  streams that are already open and to queued work.

## Decision: peer authentication (D2)

- PKIX certificate validation followed by SASL EXTERNAL (XEP-0178) is the
  primary authentication path in both directions. The reference identity is
  the stream `to` domain on outbound streams and the stream `from` domain on
  inbound streams (RFC 6120 section 13.7.2.1).
- Certificates whose extended key usage is serverAuth only are accepted in both
  TLS roles. Public CAs such as Let's Encrypt have stopped issuing clientAuth
  certificates, so requiring clientAuth from an initiating peer would refuse
  most public servers.
- `open` mode uses a public-WebPKI trust profile. Configured private CAs keep
  the strict per-peer profile of the verifier design, including complete CRLs
  with fail-closed freshness; those hard CRL checks apply only to configured
  private CAs. Revocation handling for public chains is selected and reviewed
  with the verifier implementation, and any gap against RFC 6120 section
  13.7.2 is recorded as a limitation, not claimed as conformance.
- Inbound XEP-0220 dialback, in which Waddle accepts a peer's identity by
  dialback, is off by default. When enabled, it runs only over TLS, only for
  operator-listed domains, and never after an active certificate failure: a
  peer whose certificate actively failed validation is not downgraded to
  dialback. This is stricter than XEP-0178, which allows a receiving server to
  continue with dialback when the certificate does not match. The dialback implementation defines which verification outcomes
  count as an active failure, and that classification is reviewed before
  inbound dialback can be enabled. Without DNSSEC, dialback gives only weak
  identity verification and is exposed to DNS poisoning; TLS does not make it
  equivalent to SASL EXTERNAL.
- Each verified remote principal carries the strength with which its domain
  authenticated (PKIX or dialback). Host policy never grants owner or admin
  authority to principals of a domain that authenticated only by dialback.
- Waddle implements the XEP-0220 Initiating Server and Authoritative Server
  roles, so a peer that cannot verify Waddle's certificate can still verify
  Waddle by dialback. The dialback key secret (XEP-0185) belongs to one
  deployment: every replica that can answer verification requests holds it,
  and no other deployment does.
- DANE may follow as a separate additive slice. POSH is not supported.

## Decision: discovery (D3)

- To reach domain D, look up `_xmpps-server._tcp.D` and `_xmpp-server._tcp.D`
  and order both record sets together by priority and weight (XEP-0368,
  RFC 2782). `xmpps-` targets use direct TLS with SNI set to D and, when ALPN
  is sent, `xmpp-server`; `xmpp-` targets use STARTTLS.
- A record set whose only target is "." means that service is not offered for
  D. The other record set may still be used, but A/AAAA fallback is then
  forbidden.
- Fall back to A/AAAA records for D on port 5269 only when no SRV record
  exists for D.
- The reference identity is always the source domain D. SRV targets and other
  names learned from DNS are never reference identities (RFC 9525 section
  6.1.1), and DNS never authorizes a peer or its domains.
- Refuse SRV targets that resolve to loopback or private addresses, and cap the
  number of targets tried per domain.
- An explicitly configured endpoint (RFC 6120 section 3.2.3) replaces discovery
  for its domain. It selects a socket only and never changes the reference
  identity.

## Decision: identity (D6)

- Remote accounts and room affiliations are identified by normalized bare
  JIDs; full JIDs carry bound-resource and routing provenance.
- AT Protocol home login is outside this ADR and is decided in its own issue
  and ADR. This ADR keeps one invariant for it: when a home account
  authenticates through AT Protocol, the subject that federates is a stable
  home bare JID derived from the account's DID, never from its handle. A DID
  does not move a JID or transfer grants, and federated peers need neither a
  DID nor the user's OAuth tokens.
- Waddle-controlled homes enforce durable non-reuse of retired account JIDs
  across their provisioning paths. Third-party homes cannot be required to do
  so: a home operator can impersonate or reassign its users, so grants follow
  JID semantics and trust that home. Revocation and re-verification
  procedures for a reported reassignment are part of the revocation work.

## Decision: host policy and effect boundary (D5)

PostgreSQL is the canonical host-grant authority and SpiceDB a fenced,
reconstructible projection, as proposed in
[the host-grant authority design](../planning/host-grant-authority-2047.md).
The selection is accepted on that design's conditions B1, B4, B6 and B7:
writer and reader exclusion with old-writer retirement; the actual lock
order, atomic admission and lost-commit ownership; pre-parse identity
enforcement with canonical account binding; and a restore incarnation that
survives rollback. Each condition is an implementation gate; none has passed.

The first slice evaluates admission in SQL inside the PostgreSQL acceptance
transaction, so SpiceDB is not on the safety path for that slice. SpiceDB
joins the admission path only when inheritance needs it, and then only through
the design's fenced projection protocol (its B3).

Community, room and deployment authority are distinct. A Space subscription
does not grant access to its children. The host must consistently authorize
discovery, room joins, sends, history, fanout and administration; a community
owner has no implicit deployment-wide or unrelated-community authority.
Bootstrap grants identify an exact bare account JID and named community through
secured operator provisioning, with audit and recovery.

Before protected remote dispatch or any activation, executable evidence must
show how the selected authority interacts with the other stores and replicas.
The implementation must inventory and fence *all* grant writers, including
bootstrap, Space/PubSub affiliation changes, parent repair, room affiliations,
startup reseeding, read backfill, compensation and stale reconciliation tasks.
These include read-side extension membership, legacy XMPP permission-state
writes and ad-hoc channel compensation, not only explicit membership
administration endpoints. Current reseeding can overwrite an outcast, and
Space fanout can use a subscriber list captured before a revocation; both need
explicit regression coverage at the final admission boundary. The
implementation must define:

1. The durable scope, generation, deterministic lock order and mutation
   linearization point, including moves between scopes and concurrent changes.
2. How an in-progress or unknown external-store write is recovered after
   cancellation, process death or restart without a stale writer applying an
   older grant after a newer revoke. No PostgreSQL transaction is assumed to
   atomically include SpiceDB or a live actor.
3. The final protected-effect admission boundary on every replica, including
   queued work and retries. Revoke acknowledgement must mean no new protected
   admission under the revoked grant after its specified fence; the design must
   define already admitted or externally in-flight work separately. A prior
   permission check or generic queue acceptance alone is not that proof.
4. Fail-closed behavior for unavailable authority, stale projections, an
   uncertain commit and incomplete recovery, while unrelated communities and
   local traffic continue within bounded resource limits.
5. Compatibility of existing records and writers. Replace the non-injective
   SpiceDB subject and object ID encoding before remote identity lookup;
   establish an unambiguous versioned namespace and either prove it empty or
   migrate from authoritative data with collision detection and fail closed on
   ambiguity. Do not infer identity by decoding ambiguous legacy IDs.

The grant-authority work must also fix and retire these known paths:

- The channel schema's `read`, `send_message` and `view` permissions do not
  subtract `outcast`
  ([`permissions/schema.rs`](../../crates/waddle-server/src/permissions/schema.rs)),
  so a room outcast who can view the parent Space appears to keep access.
  Outcast takes precedence in every channel action.
- Retire the Spaces owner reseeding that can overwrite an outcast, the latent
  legacy `set_room_affiliation` path, and the MUC admin write that persists
  graph state before the room actor applies it.

Tests use the production PostgreSQL authority and, wherever SpiceDB
participates, production SpiceDB. A local test tuple store establishes neither
SpiceDB nor cross-store correctness. These are blocking implementation gates,
not a claim that the existing code already enforces the boundary. PostgreSQL
remains authoritative for session lifecycle, claims and contiguous frontiers;
this ADR does not change that boundary or introduce arbitrary-node resume.

## Decision: Spaces (D4)

Waddle Spaces implement XEP-0503 pinned at version 0.3.0 (xsf/xeps
`4132c531487fe9450ae90b905b8df950d0d478b2`). The namespace `urn:xmpp:spaces:0`
is unchanged from 0.2.0, and the room disco field that 0.3.0 specifies,
`muc#roomconfig_pubsub`, is the one Waddle already emits.

- XEP-0503 is experimental. It deliberately defines no access control,
  membership or inheritance for a Space's children, and does not describe a
  Space spanning several services. A Space subscription is not a room grant;
  host policy decides room admission separately, and Waddle does not invent
  inheritance under the official namespace.
- Version 0.3.0 lets a Space carry avatar and banner items whose metadata
  points at image URLs. Waddle does not fetch those images until that
  behavior has its own review.
- A client without Spaces support reaches every accessible child room by its
  plain MUC JID.
- No Prosody or ejabberd XEP-0503 implementation exists at the time of this
  decision; the independent implementations are Movim (client) and Slidge 0.4
  beta (read-only gateway). Spaces interoperability therefore requires that a
  Waddle client uses a Space hosted on stock Prosody PubSub and on stock
  ejabberd PubSub over S2S; that a Prosody account and an ejabberd account
  read a Waddle Space with plain XEP-0060; and that every child room joins by
  its plain MUC JID. Movim and Slidge results are recorded but do not block.

## Delivery and recovery

Preserve the existing [ingress effect acceptance contract](../operations/ingress-effect-acceptance.md),
including frozen targets and child identities, the specified unknown-outcome
retry policy, and storage-hint behavior. Federated handoff must separately
define custody, retry identity, ordering scope and error responsibility. A
socket write, transport queue insertion or remote server acceptance does not
prove recipient delivery. A retry must not silently expand the original
recipient set. Pending IQs expire; reconnect and recovery do not promise
global ordering or exactly-once observation. Existing room-manifest acceptance
does not revoke already accepted children after a later policy revision. The
authorization design must classify those accepted children at the revocation
boundary and reconcile this contract with any final-release check; changing
that existing contract requires its own reviewed behavioral slice.

The release gate uses two independent native Waddle processes with separate
persistence, secrets and production-like permission authorities, normal client
authentication, and actual S2S transport. It also uses pinned Prosody and
ejabberd server processes provided through Nix, tested in both directions with
separate persistence and real account authentication. The ejabberd pin is
25.08 or later and sets `s2s_use_starttls: required`, because its default is
`false`. The gate covers peer and component trust, spoofing, relay and forged
IQs, bounded resource exhaustion, identity collisions, scoped administration,
revoke races and stale repair, concurrent replicas, partial failure, and each
deployment restarting while the other remains live. Its certificate matrix
includes serverAuth-only certificates, in the style of
`le-tlsserver.badxmpp.eu`, which must authenticate in both roles. Tests must
distinguish a remote outage from denial and show that unaffected local traffic
proceeds. Record the actual peer and client features and unsupported
combinations. Skipped dependencies do not count as passing evidence.
XEP-specific changes retain their dedicated test suites. The web pilot and
each subsequent native client require their own recorded acceptance evidence.

## Implementation gates

Each gate blocks the work it names. None has passed.

| Gate | Blocks | Source |
| --- | --- | --- |
| Handshake B1 remainder: updates and errata disposition; TLS version profile (RFC 9325 section 3.1.1 requires TLS 1.2 support, while the handshake fixture is TLS 1.3 only); DNS-ID, SRV-ID, XmppAddr and wildcard matrix; IDNA and address preparation; malformed-header behavior; dialback wire profile | S2S transport | [Handshake design](../planning/s2s-handshake-2047.md) |
| Handshake B2: concrete verifier for both trust profiles, including revocation handling for public chains | S2S transport | [Verifier design](../planning/s2s-verifier-2047.md) |
| Handshake B3: streaming codec and restart buffer ownership | S2S transport | Handshake design |
| Handshake B4: independent review of the implementation and its B1–B3 evidence | S2S transport | Handshake design |
| Host-grant B1, B4, B6 and B7, the conditions of D5 | Enrollment, first manifest integration, external identities, recovery and activation | [Host-grant design](../planning/host-grant-authority-2047.md) |
| Host-grant B2, B3 and B5 | Affected protected operations; projection-based admission; PubSub and DirectFrame sinks | Host-grant design |
| Production authorization ID codec | Remote identity lookup and grants | This ADR |
| Prosody and ejabberd negative matrix, fail-closed P2 admission and the production authorization ID codec | Changing the default mode from `allowlist` to `open` | D1 |
| Classification of active certificate failures | Enabling inbound dialback | D2 |
| Release gate evidence | Feature advertisement and activation | This ADR |
| Explicit activation decision with a named window and runbook | Enabling S2S in any deployment | This ADR |

## Protocol references

Implement and test the applicable requirements of [RFC 6120](https://www.rfc-editor.org/rfc/rfc6120),
[RFC 6121](https://www.rfc-editor.org/rfc/rfc6121),
[RFC 7590](https://www.rfc-editor.org/rfc/rfc7590),
[RFC 7622](https://www.rfc-editor.org/rfc/rfc7622),
[RFC 9325](https://www.rfc-editor.org/rfc/rfc9325), and
[RFC 9525](https://www.rfc-editor.org/rfc/rfc9525), including applicable
updates and errata. RFC 9525 obsoletes RFC 6125, which XEP-0178 cites; apply
RFC 9525.

XSF sources are pinned at xsf/xeps commit
[`4132c531487fe9450ae90b905b8df950d0d478b2`](https://github.com/xsf/xeps/tree/4132c531487fe9450ae90b905b8df950d0d478b2):

| Specification | Version | Used for |
| --- | --- | --- |
| XEP-0030 Service Discovery | 2.5.0 | Discovery across S2S |
| XEP-0045 Multi-User Chat | 1.35.5 | Inbound and outbound MUC |
| XEP-0060 Publish-Subscribe | 1.30.0 | Spaces over S2S |
| XEP-0115 Entity Capabilities | 1.6.0 | Capabilities of remote entities |
| XEP-0178 Best Practices for Use of SASL EXTERNAL with Certificates | 1.2 | PKIX authentication (D2) |
| XEP-0185 Dialback Key Generation and Validation | 1.0 | Dialback keys (D2) |
| XEP-0220 Server Dialback | 1.1.1 | Dialback policy and roles (D2) |
| XEP-0313 Message Archive Management | 1.1.3 | Host-authorized history |
| XEP-0359 Unique and Stable Stanza IDs | 0.7.0 | Message identity |
| XEP-0368 SRV records for XMPP over TLS | 1.1.0 | Discovery (D3) |
| XEP-0402 PEP Native Bookmarks | 1.2.0 | Remote room bookmarks |
| XEP-0503 Server-side spaces | 0.3.0 | Spaces (D4) |

Each slice reviews the sections it changes against these revisions. Feature
advertisement requires its own demonstrated behavior and test coverage.
