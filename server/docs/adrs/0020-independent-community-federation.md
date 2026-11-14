# ADR-0020: Open XMPP and Spaces federation

## Status

Accepted on 2026-11-13 for decisions D1–D6; D2 and D3 were extended and
D7–D9 accepted on 2026-11-14. These decisions replace the restricted
two-Waddle allowlist pilot this ADR previously proposed: the target is open,
standards-compatible XMPP federation with independently operated servers,
including Prosody and ejabberd, and XEP-0503 Spaces.

Accepting these decisions selects designs; it is not evidence that any of them
works, and none of the behavior they describe is implemented yet. None of the
[gates](#gates) below has passed. Nothing in this ADR authorizes enabling S2S,
advertising federation or any production activation; activation remains a
separate explicit decision with a named window and runbook.
[The issue 2047 plan](../../../docs/planning/federation-2047.md) sequences the
work and names, for each gate, the slice that owns it and the verification step
that discharges it.

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
| D1 | Peer admission modes `closed`, `allowlist` and `open` with a denylist. S2S is off by default; `allowlist` is the default mode until named evidence passes. Admission is enforced at stream open and at the protected-effect boundary. |
| D2 | TLS always. PKIX and SASL EXTERNAL are primary and accept serverAuth-only certificates in both roles. The trust profile is chosen per domain, public WebPKI by default. Inbound streams use STARTTLS only. Inbound dialback is restricted, Waddle will implement the outbound dialback roles, and every stream carries its authentication strength to the effect boundary. |
| D3 | SRV discovery merges `_xmpps-server` and `_xmpp-server`. The source domain is the only reference identity. Every DNS-derived address is checked against blocked address classes before connecting, and a failed lookup is not an absent record. |
| D4 | XEP-0503 is pinned at version 0.3.0; avatar and banner fetching is off. |
| D5 | PostgreSQL is the canonical grant authority and SpiceDB a fenced projection, with conditions. The first grant-authority slice evaluates admission in SQL. |
| D6 | AT Protocol login moves to its own issue and ADR. The federated subject is a stable home bare JID derived from the DID, never the handle. |
| D7 | Remote traffic to local accounts is in scope; until it is implemented, it is refused as if the account did not exist. |
| D8 | Remote principals never receive privileged grants. A reassigned third-party JID may inherit member-level grants; this is an accepted limitation. |
| D9 | Tests are written with each slice and executed once all slices are integrated, except the TLS verifier and grant-fence focused tests, which run when their slices complete. |

### Terms

- **Source domain**: as in RFC 6120 section 13.7.2.1 and RFC 9525, the domain
  a stream's certificate must prove. It is the stream `to` domain on outbound
  streams and the stream `from` domain on inbound streams, and it never comes
  from DNS.
- **Remote principal**: an account or service identity whose domain this
  deployment does not serve, verified through an S2S stream. An account homed
  on one Waddle deployment is a remote principal on every other deployment.
- **Privileged grant**: owner, admin or manager standing at deployment,
  community, room or role scope, and any other grant that confers a management
  permission. In the current
  [permission schema](../../crates/waddle-server/src/permissions/schema.rs),
  the Space `moderator` relation is such a grant because it confers
  `manage_members`.
- **Authentication strength**: `pkix` when a stream's domain pair
  authenticated by SASL EXTERNAL after certificate validation, `dialback` when
  it authenticated by XEP-0220. A stream is *dialback-only* when its strength is
  `dialback`.

## Decision: transport and addressing

- Use native XMPP S2S (`jabber:server` streams, RFC 6120) between Waddle and
  any compatible peer admitted under D1. Continue to use WebSocket for C2S; do
  not use cluster relay, a shared database, or a separate HTTP login or
  membership API as the federation protocol.
- Protect every S2S stream with TLS before dialback, SASL or any stanza.
  Inbound streams use required STARTTLS (D2). Outbound streams use required
  STARTTLS, or direct TLS for `_xmpps-server` targets (XEP-0368). No plaintext
  or unauthenticated fallback is permitted; a TLS failure never leads to a
  plaintext retry.
- Streams are directional; XEP-0288 bidirectional streams are outside this
  profile. Each stream carries exactly one authenticated source and target
  domain pair. Waddle neither accepts nor initiates XEP-0220 multiplexing of
  further sender or target domains onto a stream. Authenticating one domain, or
  a parent domain, does not authorize another domain, subdomain or component.
- [The handshake design](../planning/s2s-handshake-2047.md) and
  [the verifier design](../planning/s2s-verifier-2047.md) specify which
  component terminates TLS, verifies the peer and authorizes SASL, and how
  certificate rotation and connection resumption are revalidated. Their open
  items are gates G1–G4.
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
  completes a pending request at most once, including when the reply reaches a
  different replica from the one that sent the request. Distinguish transit IQ
  replies from requests locally originated by the server; accept legitimate
  protocol errors without satisfying unrelated requests. Return the appropriate
  bounded stream or stanza errors without error loops; do not fabricate success.
- Bound connections, stanza size and parsing, queues, retries, timeouts and
  per-peer work. A partition or malicious peer must not indefinitely block
  independent local traffic. In `open` mode anyone who controls a domain and
  can obtain a certificate for it can become a peer, so the number of peers is
  attacker-controlled: global caps on peers, sockets, handshakes and per-peer
  state, with an eviction and reconnect backoff policy, are required (G7). The
  handshake design's fixture ceilings are not that resource model.

## Decision: peer admission (D1)

- S2S is off by default; enabling it is explicit operator configuration.
- An enabled deployment runs in one admission mode. `closed` admits no remote
  domain. `allowlist` admits only operator-listed domains. `open` admits any
  domain that authenticates under D2, except operator-denylisted domains.
- `allowlist` is the default mode until all of these pass: the Prosody and
  ejabberd negative matrix, fail-closed host-grant admission, and the
  production authorization ID codec (G20). The default then becomes `open` with
  a denylist, through its own reviewed change after verification; #2047 does
  not close before that change.
- Domain admission grants transport only. An admitted domain may hold streams
  and exchange stanzas under the routing rules above; it gains no community,
  room, PubSub or administrative access. Host policy decides each of those.
- The current admission decision is enforced when a stream opens and again at
  the protected-effect admission boundary (G17). When a domain is no longer
  admitted, because it was denylisted, removed from the allowlist, or the mode
  changed to `closed`, no new protected effect is admitted for it from streams
  that are already open or from queued work, and a replica closes that
  domain's open streams when it applies the change.
- Admission lists belong to the operator's S2S configuration snapshot
  described in the handshake design. A replica applies a change when it
  restarts with the new snapshot, or through a reload mechanism that the
  connection-manager design defines and has reviewed (G7). A change is complete
  only when every replica runs it.
- List entries and stream domains are compared in one canonical form: prepared
  under RFC 7622, without a trailing dot, with a U-label domain and its A-label
  form treated as the same domain; the exact IDNA processing is part of G1. An
  entry that cannot be prepared fails configuration load. Matching is exact,
  and an entry says nothing about subdomains.
- The inbound dialback list (D2) is a separate list; being admitted does not
  permit dialback.

## Decision: peer authentication (D2)

- PKIX certificate validation followed by SASL EXTERNAL (XEP-0178) is the
  primary authentication path in both directions. The reference identity is
  the source domain.
- Certificates whose extended key usage is serverAuth only are accepted in both
  TLS roles, in every trust profile and admission mode. Let's Encrypt stopped
  issuing certificates with the clientAuth usage on 2026-07-08
  ([announcement](https://letsencrypt.org/2025/05/14/ending-tls-client-authentication/)),
  so requiring clientAuth from an initiating peer would refuse most public
  servers. Accepting a serverAuth-only certificate from a TLS client departs
  from RFC 5280 section 4.2.1.12, which limits a certificate carrying the
  extension to the purposes it lists; this is a recorded deviation.
- The trust profile is chosen per domain, independently of the admission mode.
  Public WebPKI is the default profile for every domain in every mode. An
  operator may configure a domain with a private-CA profile, which trusts only
  that profile's anchors and applies the verifier design's complete CRLs with
  fail-closed freshness; a certificate from a public CA then fails for that
  domain. The profile is selected from configuration before the handshake,
  never as a fallback after another profile rejects a chain. Revocation
  handling for public chains is selected and reviewed with the verifier (G2),
  and any gap against RFC 6120 section 13.7.2 is recorded as a limitation, not
  claimed as conformance.
- Inbound streams use STARTTLS only, so the peer's claimed domain is known
  from its initial stream header and the profile and reference identity are
  fixed before the TLS handshake. An inbound peer that omits `from` from its
  initial, pre-TLS stream header is refused before TLS. This is stricter than
  RFC 6120 section 4.7.1, which requires a server to send `from` only once the
  stream is protected, and is a recorded deviation; it keeps every certificate
  verification bound to a reference identity. Whether the pinned Prosody and
  ejabberd send a pre-TLS `from` is checked in the interoperability harness.
  Inbound direct TLS (XEP-0368) is deferred until a two-stage verification
  design is reviewed; until then no inbound direct-TLS listener exists.
  Outbound streams may use direct TLS, because Waddle knows the target domain
  before its handshake.
- Waddle validates the receiving server's certificate on every outbound
  stream, whether it then authenticates itself by SASL EXTERNAL or by
  dialback, and on every connection it opens to an Authoritative Server.
  Dialback never replaces Waddle's own PKIX check of a server it connects to.
- Inbound XEP-0220 dialback, in which Waddle accepts a peer's identity by
  dialback, is off by default. When enabled, it runs only over TLS, only for
  domains on an operator dialback list that is separate from the allowlist and
  whose domains must also be admitted under D1, and never after an active
  certificate failure: a peer whose certificate failed validation is not
  downgraded to dialback. An active certificate failure is a presented
  certificate that fails validation for the claimed domain under that domain's
  trust profile, whether it was presented by the inbound peer or by the
  Authoritative Server on Waddle's verification connection. The dialback design
  classifies the remaining outcomes, such as an absent certificate, and that
  classification is reviewed before inbound dialback can be enabled (G5). This
  is stricter than XEP-0178, which allows a receiving server to continue with
  dialback when the certificate does not match.
- Dialback gives only weak identity verification. DNS selects the
  Authoritative Server that vouches for a domain, so without DNSSEC dialback is
  exposed to DNS poisoning, and TLS does not make it equivalent to SASL
  EXTERNAL. This is the one path on which DNS influences who is authenticated
  (D3).
- Each stream records its authentication strength. A domain pair
  authenticated by dialback is `dialback` even if a certificate on that stream
  or on the verification connection validated; Waddle does not implement
  dialback that skips the key check because a certificate validated. Because a
  stream carries exactly one domain pair, the strength is a property of the
  stream. It travels with every stanza from the stream into the remote
  principal and into the protected-effect admission witness (G17). Privileged
  authority is never honored for a remote principal (D8), so in particular it
  is never honored on a dialback-only stream, whatever grants are stored. Host
  policy may additionally require `pkix` strength for a community or room.
- Waddle will implement the XEP-0220 Initiating Server and Authoritative Server
  roles, so a peer that cannot verify Waddle's certificate can still verify
  Waddle by dialback. Waddle answers verification requests only over TLS, so a
  peer that verifies dialback keys over an unencrypted connection, which
  XEP-0220 permits, cannot verify Waddle. The dialback key secret (XEP-0185)
  belongs to one deployment: every replica that can answer verification
  requests holds it, and no other deployment does.
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
  exists for D: both lookups returned a negative answer (NXDOMAIN, or a
  successful response without SRV records). A lookup failure, such as SERVFAIL,
  REFUSED, a timeout or a malformed response, is not absence: the attempt fails
  and is retried later with backoff, without A/AAAA fallback. If one record set
  returns records and the other lookup fails, the returned records are used.
  RFC 6120 section 3.2.1 says an initiating entity that receives no response to
  its SRV query SHOULD fall back; Waddle does not, because an attacker who can
  suppress DNS responses could otherwise bypass published records, including a
  "." target. This is a recorded deviation.
- The reference identity is always the source domain D. SRV targets and other
  names learned from DNS are never reference identities (RFC 9525 section
  6.1.1). DNS selects where Waddle connects; outside dialback (D2) it never
  authenticates a peer or authorizes its domains.
- Before connecting to any DNS-derived address, whether it came from an SRV
  target or from A/AAAA fallback, Waddle refuses it if it is in a blocked
  class, and connects only to the exact address it checked, so a changed DNS
  answer cannot substitute another address. The blocked classes are at least:

  | Class | IPv4 | IPv6 |
  | --- | --- | --- |
  | Unspecified | `0.0.0.0/8` | `::/128` |
  | Loopback | `127.0.0.0/8` | `::1/128` |
  | Private and unique local | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | `fc00::/7` |
  | Link-local | `169.254.0.0/16` | `fe80::/10` |
  | Shared address space (CGNAT) | `100.64.0.0/10` | — |
  | Multicast | `224.0.0.0/4` | `ff00::/8` |
  | Broadcast, reserved and documentation | `255.255.255.255/32`, `240.0.0.0/4`, `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24` | `2001:db8::/32` |
  | Embedded IPv4 | — | IPv4-mapped `::ffff:0:0/96` and NAT64 `64:ff9b::/96` addresses whose embedded IPv4 address is in a blocked class |

  The discovery design completes this list from the IANA special-purpose
  address registries: an address those registries mark as not globally
  reachable is blocked unless the design records why not (G6).
- Only an explicitly configured endpoint (RFC 6120 section 3.2.3) is exempt
  from these address checks. It replaces discovery for its domain, selects a
  socket only and never changes the reference identity.
- Cap the number of SRV targets tried per domain and the number of addresses
  tried per target; the discovery design fixes both values (G6).

## Decision: identity (D6)

- Remote accounts and room affiliations are identified by normalized bare
  JIDs; full JIDs carry bound-resource and routing provenance.
- AT Protocol home login is outside this ADR and is decided in its own issue
  and ADR. This ADR keeps one invariant for it: when a home account
  authenticates through AT Protocol, the subject that federates is a stable
  home bare JID derived from the account's DID, never from its handle. As
  issue #2047 states, "A DID does not automatically move a JID or transfer
  grants"; account linking, home migration and grant rebinding need their own
  reviewed design. Federated peers need neither a DID nor the user's OAuth
  tokens.

## Decision: traffic to local accounts (D7)

- Remote traffic addressed to local accounts is in scope: RFC 6121 messages,
  presence, subscription requests and probes, and IQs addressed to an
  account's bare or full JID. It follows RFC 6121's server rules. The work
  starts from an inventory of every local-account handler a remote sender can
  reach; a handler that assumes a local session is unreachable by a remote
  principal until it has been reviewed for one. Local accounts' matching
  outbound traffic, such as presence to remote contacts, direct messages and
  subscription requests, uses the same S2S egress as other traffic.
- Until that work lands, remote traffic to a local account is refused as if
  the account did not exist (RFC 6121 section 8.5.1): IQ requests and messages
  receive a `service-unavailable` stanza error, presence of every type is
  ignored, and no error is returned for an error stanza. Two kinds of traffic
  are excepted: IQ results and errors that correlate with a request the account
  sent, and traffic from a remote room to a local occupant's full JID that
  correlates with that occupant's join.

## Decision: remote grants and reassigned JIDs (D8)

- Remote principals never receive privileged grants. Grant issuance, bootstrap
  and repair refuse them, and the protected-effect admission boundary does not
  honor privileged authority for a remote principal, whatever is stored (G17).
  A deployment, community or room is therefore administered only by accounts
  homed on the deployment that hosts it; remote accounts hold only
  non-privileged grants.
- Waddle-controlled homes enforce durable non-reuse of retired account JIDs
  across all their provisioning paths.
- Third-party homes cannot be required to do so: a home operator can
  impersonate or reassign its users, and grants follow JID semantics and trust
  that home. A re-registered remote account, or a domain that changes hands,
  may therefore inherit the member-level grants of the JID's previous holder.
  This is an accepted, recorded limitation.
- Waddle does not detect reassignment. When a reassignment is reported, a host
  administrator revokes the JID's grants through the revocation fence. Access
  for the new holder is a new grant, issued only after the administrator has
  re-verified the holder with the remote home's operator outside the protocol.

## Decision: host policy and effect boundary (D5)

PostgreSQL is the canonical host-grant authority and SpiceDB a fenced,
reconstructible projection, as proposed in
[the host-grant authority design](../planning/host-grant-authority-2047.md).
The selection is accepted on that design's conditions B1, B4, B6 and B7:
writer and reader exclusion with old-writer retirement; the actual lock
order, atomic admission and lost-commit ownership; pre-parse identity
enforcement with canonical account binding; and a restore incarnation that
survives rollback. They are gates G10–G13; none has passed.

The first grant-authority slice is the design's proposed first slice: one
fresh isolated community, a managed members-only room and explicit
affiliations, admitted through room-manifest acceptance. It evaluates admission
in SQL inside the PostgreSQL acceptance transaction, so SpiceDB is not on the
safety path for that slice. SpiceDB joins the admission path only when
inheritance needs it, and then only through the design's fenced projection
protocol (its B3, gate G15).

Community, room and deployment authority are distinct. A Space subscription
does not grant access to its children. The host must consistently authorize
discovery, room joins, sends, history, fanout and administration; a community
owner has no implicit deployment-wide or unrelated-community authority.
Bootstrap grants identify an exact bare account JID and named community through
secured operator provisioning, with audit and recovery; a remote JID can be
bootstrapped only to non-privileged grants (D8).

Before protected remote dispatch is enabled or any activation, executable
evidence must show how the selected authority interacts with the other stores
and replicas. The implementation must inventory and fence *all* grant writers,
including bootstrap, Space/PubSub affiliation changes, parent repair, room
affiliations, startup reseeding, read backfill, compensation and stale
reconciliation tasks. These include read-side extension membership, legacy XMPP
permission-state writes and ad-hoc channel compensation, not only explicit
membership administration endpoints. Current reseeding can overwrite an
outcast, and Space fanout can use a subscriber list captured before a
revocation; both need explicit regression coverage at the final admission
boundary. The implementation must define:

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
   permission check or generic queue acceptance alone is not that proof. The
   admission witness binds the principal, either a local account or a remote
   bare JID with its verified domain and authentication strength, and the
   current D1 admission decision for a remote domain, alongside the canonical
   policy state.
4. Fail-closed behavior for unavailable authority, stale projections, an
   uncertain commit and incomplete recovery, while unrelated communities and
   local traffic continue within bounded resource limits.
5. Compatibility of existing records and writers. Replace the non-injective
   SpiceDB subject and object ID encoding before remote identity lookup;
   establish an unambiguous versioned namespace and either prove it empty or
   migrate from authoritative data with collision detection and fail closed on
   ambiguity. Do not infer identity by decoding ambiguous legacy IDs.

The grant-authority work must also fix and retire these known paths:

- None of the channel permissions in the
  [permission schema](../../crates/waddle-server/src/permissions/schema.rs)
  (`read`, `send_message`, `view`, `moderate`, `manage` and `delete`) subtracts
  `outcast`, so a room outcast who can view the parent Space appears to keep
  access. Outcast takes precedence in every channel action. This is a behavior
  change and is made in its own reviewed change. Every replica currently writes
  its built-in SpiceDB schema at startup, so an older replica would revert the
  fix; the change therefore needs a fenced schema transition, in which startup
  compares a versioned schema and refuses to overwrite a different one, and old
  binaries are retired before the new schema is relied on.
- Retire the Spaces owner reseeding that can overwrite an outcast, the latent
  legacy `set_room_affiliation` path, and the MUC admin write that persists
  graph state before the room actor applies it.

Tests use the production PostgreSQL authority and, wherever SpiceDB
participates, production SpiceDB. A local test tuple store establishes neither
SpiceDB nor cross-store correctness. These requirements are gates G10–G17, not
a claim that the existing code already enforces the boundary. PostgreSQL
remains authoritative for session lifecycle, claims and contiguous frontiers;
this ADR does not change that boundary or introduce arbitrary-node resume.

## Decision: Spaces (D4)

Waddle Spaces will conform to XEP-0503 version 0.3.0 (xsf/xeps
`4132c531487fe9450ae90b905b8df950d0d478b2`). The namespace `urn:xmpp:spaces:0`
is unchanged from 0.2.0, and the room disco field that 0.3.0 specifies,
`muc#roomconfig_pubsub`, is the field the current code emits
([`muc/roominfo.rs`](../../crates/waddle-xmpp/src/muc/roominfo.rs)); the
[XEP-0503 module](../../crates/waddle-xmpp/src/xep/xep0503.rs) header still
names 0.2.0.

- XEP-0503 is experimental. It deliberately defines no access control,
  membership or inheritance for a Space's children, and does not describe a
  Space spanning several services. A Space subscription is not a room grant;
  host policy decides room admission separately, and Waddle does not invent
  inheritance under the official namespace.
- Version 0.3.0 lets a Space carry avatar and banner items whose metadata
  points at image URLs. Neither the server nor Waddle clients fetch those
  images until that behavior has its own review.
- A client without Spaces support reaches every accessible child room by its
  plain MUC JID.
- When this decision was made, no Prosody or ejabberd XEP-0503 module was
  found, and the independent implementations found were Movim (client) and
  Slidge 0.4 beta (read-only gateway). That survey is unverified and is
  repeated before Spaces acceptance is fixed. On that basis, Spaces
  interoperability requires that a Waddle client uses a Space hosted on stock
  Prosody PubSub and on stock ejabberd PubSub over S2S; that a Prosody account
  and an ejabberd account read a Waddle Space with plain XEP-0060; and that
  every child room joins by its plain MUC JID. Movim and Slidge results are
  recorded but do not block.

## Decision: verification timing (D9)

- Each slice writes and compiles its tests but does not execute them. Tests
  run once every slice has merged into the shared integration branch, in the
  verification phase the plan defines, cheapest step first.
- Exception: the focused test binaries of the TLS verifier, of the first
  grant-authority slice and of the revocation fence run as soon as their slice
  is complete, and must pass before slices that build on them merge into the
  integration branch. These are the parts most likely to compile and still be
  wrong.
- A slice merges into the integration branch once its design decisions (see
  [gates](#gates)) are recorded and independently reviewed and its static
  checks pass. Slices merge into main only after the verification phase has
  discharged the evidence gates that cover them and the close-out review of the
  integrated head is clean.
- Skipped prerequisites are recorded as unverified, never as passed.
- For this work, D9 replaces the repository's usual expectation that each
  change's tests pass before it merges. The plan records the risk of finding
  defects late.

## Delivery and recovery

Preserve the existing [ingress effect acceptance contract](../operations/ingress-effect-acceptance.md),
including frozen targets and child identities, the specified unknown-outcome
retry policy, and storage-hint behavior. Federated handoff must separately
define custody, retry identity, ordering scope and error responsibility (G8).
A socket write, transport queue insertion or remote server acceptance does not
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
25.08 or later and sets `s2s_use_starttls: required`; ejabberd documents
`false` as the default, and the harness records the effective setting of the
pinned version. The gate covers peer and component trust, spoofing, relay and
forged IQs, discovery address restrictions, bounded resource exhaustion,
identity collisions, scoped administration, revoke races and stale repair,
concurrent replicas, partial failure, and each deployment restarting while the
other remains live. Its certificate matrix includes serverAuth-only
certificates issued by the harness CA, modelled on the public
`le-tlsserver.badxmpp.eu` test endpoint, which the native harness does not
contact; they must authenticate in both roles. It also includes certificates
from the wrong trust profile for a domain, which must fail.

The harness uses loopback addresses, `.test` domains and explicitly configured
endpoints, which D3 exempts from discovery and its address checks. A dedicated
discovery suite with a stub resolver therefore covers D3's ordering, fallback,
failure, address-class, rebinding and cap rules. The public-WebPKI profile is
tested in the verifier suite with a substituted test root set; validation
against the real public root store is evidence for the activation decision
(G22). Tests must distinguish a remote outage from denial and show that
unaffected local traffic proceeds. Record the actual peer and client features
and unsupported combinations. XEP-specific changes retain their dedicated test
suites. The web pilot and each subsequent native client require their own
recorded acceptance evidence.

The federation journeys in
[the product gate](../../../docs/product/critical-journeys.json) keep their own
gate. Evidence from this work contributes to them but does not by itself
satisfy them; at least the Snikket peer, file exchange in remote rooms, message
moderation, the journeys' ordered and deduplicated delivery wording, and
clients other than the web pilot remain unverified until their own evidence
exists.

## Gates

Each gate is one of three kinds:

- A **decision** gate is an open design question. The slice that owns it
  decides it, records the decision in the named design document and has it
  independently reviewed before that slice merges into the integration branch;
  an unresolved blocking review finding pauses the merge. Under D9 no decision
  gate blocks authoring.
- An **evidence** gate is discharged by executing the named tests in the
  verification phase, or at slice completion for D9's exceptions. It blocks
  merging the affected slices into main and every enablement that depends on
  it.
- An **enablement** gate blocks turning on a behavior, a default or an
  advertisement until its named evidence has passed and the change has been
  reviewed.

Several gates have both a decision and an evidence part. The plan names the
owning slice and the verification step for each.

| Gate | Kind | Blocks | Source |
| --- | --- | --- | --- |
| G1 Transport standards profile (handshake B1 remainder): updates and errata disposition; TLS version profile (RFC 9325 section 3.1.1 requires TLS 1.2 support, while the handshake fixture is TLS 1.3 only); DNS-ID, SRV-ID, XmppAddr and wildcard matrix; IDNA and address preparation; remaining SASL conformance; malformed-header and stream-error behavior, including the required pre-TLS `from` | Decision, evidence | Merge of the transport slices; then merge to main | [Handshake design](../planning/s2s-handshake-2047.md) |
| G2 Concrete verifier (handshake B2) for both trust profiles: public-chain revocation handling, algorithm intersection, parser and CRL ceilings, cancellation and resource bounds | Decision, evidence | Merge of the verifier slice; slices that build on it (D9 exception); merge to main | [Verifier design](../planning/s2s-verifier-2047.md) |
| G3 Streaming codec and restart-buffer ownership (handshake B3) | Decision, evidence | Merge of the codec slice; merge to main | Handshake design |
| G4 Independent review of the transport design with its G1–G3 decisions (handshake B4) | Decision | Merge of the connection manager, the first slice that accepts sockets | Handshake design |
| G5 Dialback wire profile and classification of active certificate failures | Decision, enablement | Merge of the dialback slice; enabling inbound dialback | D2 |
| G6 Discovery profile: completed blocked-address list, caps, failure handling; lifting the loopback and `.test` restriction on listeners, endpoints and domains | Decision, evidence | Merge of the discovery slice; any configuration outside loopback and `.test` | D3, handshake design |
| G7 Connection-manager admission and resource model: admission configuration and its distribution to replicas; global peer, socket and handshake caps with eviction and backoff | Decision, evidence | Merge of the connection manager | D1, transport |
| G8 Federated handoff contract: custody, retry identity, ordering scope, error responsibility | Decision, evidence | Merge of S2S routing | Delivery and recovery |
| G9 Production authorization ID codec | Evidence | Remote identity lookup and grants; merge to main; the D1 default change | D5 item 5 |
| G10 Host-grant B6 beyond the codec: XML-decoded identity enforcement before lossy parsing, canonical account binding, reconciliation, and the identity profile | Decision, evidence | Merge of S2S routing and of the first grant-authority slice; external identities and enrolled authority keys | [Host-grant design](../planning/host-grant-authority-2047.md) |
| G11 Host-grant B1: writer and reader exclusion, old-writer retirement, store capabilities, credential separation and old-binary retirement | Decision, evidence, enablement | Merge of the grant-authority slices; enrollment; activation | Host-grant design |
| G12 Host-grant B4: lock order, atomic admission, cancellation and lost-commit ownership | Decision, evidence | Merge of the first grant-authority slice; slices that build on it (D9 exception) | Host-grant design |
| G13 Host-grant B7: restore incarnation surviving rollback and excluding old deployments and work | Decision, evidence, enablement | Merge of the restore slice; recovery capability and activation | Host-grant design |
| G14 Host-grant B2: complete action, negative, admin and global dependency policy, including outcast precedence and privileged grants; actor projection; XMPP request and result identity | Decision, evidence | Merge of each slice that protects an operation | Host-grant design, D5, D8 |
| G15 Host-grant B3: CAS, marker and schema protocol, including the fenced schema transition; bounded reconstruction | Decision, evidence | Merge of slices that write the projection or the schema; any admission that reads the projection | Host-grant design |
| G16 Host-grant B5: PubSub and DirectFrame admission identity; read, join and stream-management sink contracts | Decision, evidence | Merge of the slice that fences those sinks; community activation | Host-grant design |
| G17 Effect-boundary admission for remote principals: current D1 admission, no privileged authority, authentication strength | Decision, evidence | Merge of the first slice that admits remote principals to protected effects | D1, D2, D8 |
| G18 Remote traffic to local accounts: RFC 6121 server rules and handler inventory | Decision, evidence | Lifting the D7 refusal | D7 |
| G19 Release gate evidence and a clean close-out review of the integrated head | Evidence | Merge to main; feature advertisement; activation | This ADR |
| G20 Prosody and ejabberd negative matrix, fail-closed host-grant admission and the production authorization ID codec | Enablement | Changing the default mode from `allowlist` to `open` | D1 |
| G21 Reviewed advertisement change limited to demonstrated behavior | Enablement | Adding any federation feature to `capabilities.toml` or disco | This ADR |
| G22 Explicit activation decision with a named window and runbook, including retirement of old binaries and credentials and a check of the public-WebPKI profile against a real public peer | Enablement | Enabling S2S in any deployment | This ADR |

## Protocol references

Slices implement and test the applicable requirements of
[RFC 6120](https://www.rfc-editor.org/rfc/rfc6120),
[RFC 6121](https://www.rfc-editor.org/rfc/rfc6121),
[RFC 7590](https://www.rfc-editor.org/rfc/rfc7590),
[RFC 7622](https://www.rfc-editor.org/rfc/rfc7622),
[RFC 9325](https://www.rfc-editor.org/rfc/rfc9325), and
[RFC 9525](https://www.rfc-editor.org/rfc/rfc9525), including applicable
updates and errata, as published by the RFC Editor. RFC 9525 obsoletes
RFC 6125, which XEP-0178 cites; apply RFC 9525.

XSF sources are pinned at xsf/xeps commit
[`4132c531487fe9450ae90b905b8df950d0d478b2`](https://github.com/xsf/xeps/tree/4132c531487fe9450ae90b905b8df950d0d478b2).
The repository's `xeps/` directory is an untracked local cache: `cuenv.lock`
checks it out at xsf/xeps `fa21a8371a078ca3a748a2a270d1b23f6ba51569`, which is
not this pin, so a slice that reads specifications from it first fetches the
pinned commit.

| Specification | Version | Used for |
| --- | --- | --- |
| XEP-0030 Service Discovery | 2.5.0 | Discovery across S2S |
| XEP-0045 Multi-User Chat | 1.35.5 | Inbound and outbound MUC |
| XEP-0060 Publish-Subscribe | 1.30.0 | Spaces over S2S |
| XEP-0115 Entity Capabilities | 1.6.0 | Capabilities of remote entities |
| XEP-0178 Best Practices for Use of SASL EXTERNAL with Certificates | 1.2 | PKIX authentication (D2) |
| XEP-0185 Dialback Key Generation and Validation | 1.0 | Dialback keys (D2) |
| XEP-0199 XMPP Ping | 2.0.1 | Ping across S2S |
| XEP-0220 Server Dialback | 1.1.1 | Dialback policy and roles (D2) |
| XEP-0280 Message Carbons | 1.0.1 | Carbons of messages exchanged with remote accounts |
| XEP-0313 Message Archive Management | 1.1.3 | Host-authorized history |
| XEP-0359 Unique and Stable Stanza IDs | 0.7.0 | Message identity |
| XEP-0368 SRV records for XMPP over TLS | 1.1.0 | Discovery (D3) |
| XEP-0402 PEP Native Bookmarks | 1.2.0 | Remote room bookmarks |
| XEP-0410 MUC Self-Ping (Schrödinger's Chat) | 1.1.0 | Membership checks in remote rooms |
| XEP-0503 Server-side spaces | 0.3.0 | Spaces (D4) |

Each slice reviews the sections it changes against these revisions. Feature
advertisement requires its own demonstrated behavior and test coverage.
