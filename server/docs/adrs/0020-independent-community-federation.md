# ADR-0020: Independent community federation pilot

## Status

Proposed. This ADR defines the restricted pilot and its implementation gates.
It does not establish that those gates have passed or authorize deployment.
The delivery sequence is in [the issue 2047 plan](../../../docs/planning/federation-2047.md).

## Context

A user authenticated to one home deployment should be able to use communities
hosted by another independently administered Waddle deployment in the same
client session. The home server authenticates the account and its bound
resources. The resource host decides access to its own communities and rooms.
Each deployment has its own persistence, secrets and administrative authority.

At the reviewed baseline `db6e5338170d71391b69bc80e57626eb13072365`, the
deployable server exposes WebSocket C2S and does not provide active S2S
transport. The existing cluster relay operates within one deployment; it is
not a federation trust boundary. The IQ dispatch currently consumes result and
error IQs before its generic local full-JID forwarding path, and the active
foreign-message path drops remote bare recipients. Neither path can be reused
as proof of remote delivery. Federation is not advertised as supported in
`server/capabilities.toml`.

Production permission actors require SpiceDB; the local tuple backend is a test
backend. This supersedes ADR-0009's statement that the implementation uses no
external authorization service, without changing its ReBAC model. Production
SpiceDB reads are fully consistent, but that does not serialize an authorization
check against a later effect or atomically coordinate PubSub storage, actors and
PostgreSQL. The existing SpiceDB ID substitution is not injective for all valid
JIDs. Both issues must be resolved before remote grants can be used.

## Decision: restricted wire and identity profile

- Use native XMPP S2S between explicitly configured, mutually permitted
  deployment and service domains. Continue to use WebSocket for C2S; do not
  use cluster relay, a shared database, or a separate HTTP login or membership
  API as the federation protocol.
- The initial two-Waddle pilot uses two independently authenticated
  directional connections per communicating domain pair, with RFC 6120
  STARTTLS and SASL EXTERNAL; XEP-0288 bidirectional streams are outside
  this pilot. No insecure or unauthenticated fallback
  is permitted. DNS discovery identifies an endpoint; it does not by itself
  authorize that endpoint or its served domains. The deployment configuration
  must explicitly map each permitted account and component domain to its
  authenticated peer in each direction; authenticating a parent domain does
  not authorize arbitrary subdomains. Specify which component terminates TLS,
  verifies the peer and authorizes SASL, and how certificate rotation and
  connection resumption are revalidated before implementation. Validate
  certificates and XMPP reference identities under the applicable specifications.
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
  independent local traffic. Enabling or advertising the transport is a
  separate explicit production decision with a named window and runbook.

The profile is deliberately narrower than general XMPP federation. In
particular, it does not satisfy the broader interoperability product gate,
which includes other servers and additional transport and product features.
Keep that gate unchanged and unverified until its own evidence exists.

## Decision: host policy and effect boundary

A [candidate PostgreSQL authority and admission design](../planning/host-grant-authority-2047.md)
records a proposal, counterexamples, implementation prerequisites and a first
vertical slice for review. Its blocking ledger remains unresolved. This ADR
stays Proposed; linking the candidate does not select an approved authority or
authorize implementation or activation.

Community, room and deployment authority are distinct. A Space subscription
does not grant access to its children. The host must consistently authorize
discovery, room joins, sends, history, fanout and administration; a community
owner has no implicit deployment-wide or unrelated-community authority.
Bootstrap grants identify an exact bare account JID and named community through
secured operator provisioning, with audit and recovery. The controlled pilot
must prevent a reassigned account JID inheriting an old grant or resolve the
grant lifecycle before release.

Before protected remote dispatch or any pilot activation, a reviewed design and
executable evidence must select one grant authority and define its interaction
with the other stores and replicas. The design must inventory and fence *all*
grant writers, including bootstrap, Space/PubSub affiliation changes, parent
repair, room affiliations, startup reseeding, read backfill, compensation and
stale reconciliation tasks. These include read-side extension membership,
legacy XMPP permission-state writes and ad-hoc channel compensation, not only
explicit membership administration endpoints. Current reseeding can overwrite
an outcast, and
Space fanout can use a subscriber list captured before a revocation; both need
explicit regression coverage at the final admission boundary. It must define:

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

The design and its tests must use the production permission backend. A local
test tuple store does not establish SpiceDB or cross-store correctness. These
are blocking implementation gates, not a claim that the existing code already
enforces the boundary. PostgreSQL remains authoritative for session lifecycle,
claims and contiguous frontiers; this ADR does not change that boundary or
introduce arbitrary-node resume.

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

The release gate uses two independent native server processes with separate
persistence, secrets and production-like permission authorities, normal client
authentication, and actual S2S transport. It covers peer and component trust,
spoofing, relay and forged IQs, bounded resource exhaustion, identity collisions,
scoped administration, revoke races and stale repair, concurrent replicas,
partial failure, and each deployment restarting while the other remains live.
Tests must distinguish a remote outage from denial and show that unaffected
local traffic proceeds. Skipped dependencies do not count as passing evidence.
XEP-specific changes retain their dedicated test suites. The web pilot and
each subsequent native client require their own recorded acceptance evidence.

## Protocol references

Implement and test the applicable requirements of [RFC 6120](https://www.rfc-editor.org/rfc/rfc6120),
[RFC 6121](https://www.rfc-editor.org/rfc/rfc6121),
[RFC 7590](https://www.rfc-editor.org/rfc/rfc7590),
[RFC 7622](https://www.rfc-editor.org/rfc/rfc7622),
[RFC 9325](https://www.rfc-editor.org/rfc/rfc9325), and
[RFC 9525](https://www.rfc-editor.org/rfc/rfc9525), including applicable
updates and errata. Review the official XSF sources for XEP-0030, XEP-0045,
XEP-0060, XEP-0313, XEP-0359, XEP-0402 and XEP-0503 for the changes that use
them, and XEP-0178 for SASL EXTERNAL. XEP-0503 is experimental and does not
define inherited access control. The reviewed sources are
[XEP-0178 1.2](https://github.com/xsf/xeps/blob/4132c531487fe9450ae90b905b8df950d0d478b2/xep-0178.xml)
and [XEP-0503 0.3.0](https://github.com/xsf/xeps/blob/4132c531487fe9450ae90b905b8df950d0d478b2/xep-0503.xml).
The other feature specifications need their own substantive review and an
exact source revision when implemented.
Feature advertisement requires its own demonstrated behavior and test coverage.
