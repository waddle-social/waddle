# Canonical PubSub admission for enrolled Spaces

**Status:** proposed. This design needs an architecture decision before
changing an enrolled Space’s admission behavior.

[Issue #2047](https://github.com/waddle-social/waddle/issues/2047) requires
host-authorized private Space discovery and event delivery. The enrolled scope key already contains the canonical service JID
and opaque node. The current ledger records only room affiliations, and
the server hides an enrolled Space while its authority is undefined.

## Proposed authority

Add a Space affiliation resource to the existing PostgreSQL host-grant
ledger, keyed by scope lifecycle and canonical bare subject JID. Enrollment
takes explicit Space bootstrap affiliations in the same transaction as the
scope and room grants. A Space affiliation mutation uses the same operation
idempotency, generation increment, verified runtime role and revocation
acknowledgment as room mutations. Preserve a retired lifecycle as a tombstone.
Treat existing enrolled scopes with no Space bootstrap as inaccessible until
an audited operator operation grants initial Space ownership. Never infer
ownership from a child room, the legacy PubSub affiliation table or SpiceDB.

Keep PubSub subscription rows as delivery preferences. A persistent row alone
confers no read or delivery authority. Explicit Space affiliations supply
XEP-0060 standing for whitelist resources. Use the whitelist access model for the first enrolled profile. XEP-0503
v0.3.0 permits open public nodes and either authorize or whitelist private
nodes. Child rooms keep independent XEP-0045
affiliations.

A local bound subject and a verified S2S subject both resolve through the
existing principal policy. A foreign member needs grant-eligible provenance.
Privileged foreign affiliations require the existing trusted-home PKIX
policy. Outcasts remain denied independently of other affiliations.

## Effect boundary

Resolve enrollment by the exact service and node, including lifecycle.
Obtain a Space decision under the replica lease before reading canonical
standing, and bind its ticket to the scope generation and principal witness.
Require a live ticket and revalidate the witness at each effect boundary.
For local and S2S IQ replies this includes admission to the response sink,
not just construction of a result. Use the same canonical check for service
list entries, node metadata, item reads, subscribe, and notification delivery
to each recipient. If storage cannot make a write conditional on the same
generation, put the write under the canonical transaction or leave that
operation unavailable. On fanout, reauthorize at dequeue and final network
handoff. A queued notification, an old subscription, and a stale membership
projection cannot authorize a new effect after revoke acknowledgment.

Route XEP-0060 affiliation changes through the canonical mutation path.
Keep generic owner commands, administration, reseed, and legacy PubSub
affiliation writes closed for enrolled nodes until each is covered by the
same authority. Serialize lifecycle and grant changes with the existing
fence. Preserve uniform private discovery denial and deny when the grant
store or replica lease cannot establish standing.

## Evidence required before enabling

- A qualified foreign member can discover, read, subscribe and receive
  events on a whitelisted Space. A foreign sibling with the same localpart,
  a member of an unrelated Space and an outcast cannot.
- A Space affiliation grants no room access. A room affiliation grants no
  Space access. Only the explicit Space owner can change its affiliations.
- Legacy PubSub writes, topology seed, projection repair and stale
  subscriptions cannot restore access. An incompatible or retired lifecycle
  cannot reuse a previous decision.
- Pause live replies, writes and queued notifications around a revoke on
  two replicas. Verify that revocation acknowledgment excludes later
  protected effects and that unrelated Spaces remain available.
- Repeat with normal WebSocket sessions and an authenticated independent
  S2S peer. Verify XEP-0060 and XEP-0503 wire behavior against pinned sources.

[XEP-0060](https://xmpp.org/extensions/xep-0060.html) and
[XEP-0503](https://xmpp.org/extensions/xep-0503.html) define the wire behavior.

## Constraints from the architecture review

The first enrolled profile is immutable whitelist. Bind the node creation
identity, access model and federation exposure to the canonical lifecycle.
Fail closed on disagreement with legacy configuration. Before enrollment,
exclude legacy schema writers and binaries and include the new state in
restore verification. Open enrolled nodes require a separate canonical
policy transition.

Hold an effect permit across the synchronous transfer at the final sink.
For asynchronous downstream ownership, carry the decision context and
require fresh admission at final transfer. Cover stream-management replay,
last-item replay, aggregate discovery and S2S reconnect. Acknowledge only
after affected transfers are fenced. Bytes already transferred and remote
copies are outside the host's subsequent control.

For owner commands, serialize authority revalidation and the protected write
under the same lifecycle lock. Make a bounded multi-affiliation request
atomic. Replay identity includes the initiator, service and lifecycle, and
a stored result may be reused only for the same normalized request.
Reject removing the last owner through the wire; audited operator recovery
uses the canonical transaction. Bind subscription preferences to the node
lifecycle and the grant era. Revocation invalidates them; a later grant
requires a new subscription. Reevaluate the subscriber's standing and
relevant route assurance at delivery.

A read and notification slice establishes only those behaviors. Full service
claims require the applicable XEP-0503 service operations, including node
creation, deletion and configuration, item retraction, and subscription
management. The canonical grant path applies to each privileged operation.
