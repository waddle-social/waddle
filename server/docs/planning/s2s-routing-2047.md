# Issue 2047: S2S routing, IQ correlation and remote reachability

## Status and scope

Decisions of the S2S routing slice (W5a) of
[the delivery plan](../../../docs/planning/federation-2047.md): the federated
handoff contract (gate G8), the inventory of what a stanza from an S2S stream
can reach (gate G23), the identity profile applied at S2S ingress (gate G10),
the interim refusal of account traffic in both directions (ADR-0020 D7) and
the local services remote senders reach (D10). It also settles two routing
follow-ups the connection manager recorded: stanza addresses written to
peers in U-labels, and a `jid-malformed` stanza error, with the stream kept,
for a valid address in a spelling the identity profile refuses.

The outbound MUC slice (W6b) adds D7's room exception in both directions:
local resources join, use and leave rooms on remote domains, and the rooms'
traffic reaches them ([Outbound MUC](#outbound-muc-w6b)).

Nothing here is executed evidence. The evidence is written and compiled but
not run until verification (D9):

- `crates/waddle-server/tests/s2s_routing.rs`: egress, refusals (the
  router's own refusal of a destination that cannot name a peer included),
  failures, bounds per resource and per remote domain, the reply share of
  the pair queues, keys too long to correlate, credential stripping, an uncorrelated local response dropped
  without dialling, and the remote principal;
- `crates/waddle-server/tests/s2s_iq_correlation.rs`: at-most-once
  completion, expiry, a response reaching another replica and its prompt
  release on the sending replica, a response carrying an address outside
  the identity profile, pruning only for stopped replicas, and keys too
  long for the table;
- `crates/waddle-server/tests/s2s_d7_refusal.rs`: the inbound D7 refusal;
- `crates/waddle-server/tests/s2s_reachability.rs`: the D10 matrix and the
  identity profile at ingress;
- `crates/waddle-server/tests/xep0030_s2s_server_discovery.rs` and
  `xep0199_s2s_ping.rs`: discovery and ping for remote askers;
- `crates/waddle-server/tests/destination_classifier.rs` and
  `rfc6120_destination_routing_ws.rs`: the RFC 6120 §10.4.3 errors for
  client messages and presence to unserved domains;
- `crates/waddle-server/tests/dm_pinned_messages_ws.rs`: a DM pin marker
  for a remote peer answered as a message to an unserved domain;
- `crates/waddle-server/tests/xep0054_0049_0191_ws.rs`: a request to a
  remote address on the sender's blocking list answered with
  `not-acceptable` (XEP-0191 §3.3);
- `crates/waddle-server/tests/s2s_connection_manager/limits.rs`: a peer's
  `conflict` neither backs the domain off nor counts as a peer failure;
- `crates/waddle-server/tests/xep0045_s2s_outbound_muc.rs`: joining,
  presence, groupchat, subject and private messages, nickname changes,
  joining again, leaving and leaving then joining again, a refused join,
  service verification (a fake join to an account refused and never sent, a
  JID that does not answer), a room's message while joining, a join the
  room never answers, the bound on rooms, a XEP-0313 query to a joined
  room, removal with status 333 when the stream fails, leaving when a
  resource ends, two replicas sharing the occupancy table, and D7 still
  refusing account traffic and traffic from rooms never joined;
- `crates/waddle-server/tests/xep0280_remote_room_carbons_ws.rs`: sent
  and received carbons of remote room private messages (XEP-0280 §6.1);
- `crates/waddle-server/tests/s2s_stream_end.rs`: which ends of an
  established stream are lost and which closed;
- `crates/waddle-server/tests/xep0410_s2s_self_ping.rs`: self-ping to a
  remote room, joined, dropped by the room, never joined, and reflected;
- `crates/waddle-xmpp/tests/xep0178_s2s_negotiation.rs`,
  `s2s_stream_addresses.rs`, `s2s_pre_authentication.rs` and
  `s2s_codec.rs`: stanzas on established streams, U-label addresses, the
  delivery of refused spellings and the content namespace mapping.

The implementation is `crates/waddle-server/src/s2s/route.rs` and `route/`,
the outbound channel of `crates/waddle-server/src/s2s/conn/` (`outbox.rs`
and the session), the codec changes in `crates/waddle-xmpp/src/s2s/`
(`content.rs`, `stream.rs`, the receiving negotiation), `destination.rs`,
the WebSocket call sites (`handlers/unserved_route.rs`, `frame.rs`, the
presence handler, `s2s_delivery.rs`, the blocking check in
`handlers/iq/destination_route.rs`), the wiring in `server/http.rs` and
`server/lifecycle.rs`, migration 2003 (`s2s_iq_correlations` and
`s2s_iq_correlation_owners`) and migration 2004
(`s2s_remote_room_occupancies` and `s2s_remote_room_verdicts`).

## The router

With S2S enabled, startup builds one `S2sRouter` per process. It is the
`StanzaSink` of every inbound stream and the `RemoteRoute` of every local
entity, and it holds:

- the S2S configuration snapshot (local domains and peer admission);
- the domains this deployment serves, built exactly as the WebSocket
  destination classifier builds them;
- the IQ correlation table in the global database, which every replica
  shares, and its own owner name on the rows it registers;
- the connection manager's handle, for dialling and writing, and the
  WebSocket delivery path to local resources, both attached after startup
  creates them.

While S2S is off no router exists, the remote route stays
`FederationDisabled` and nothing below runs.

## Ingress (G10)

The codec hands on a stanza only after reading its `to` and `from` with the
identity profile, checking `to` against the stream's local domain and
`from` against the stream's authenticated remote domain, and rewriting both
in canonical form (RFC 6120 §4.9.3; handshake design). The router then:

1. maps the stanza's content namespace from `jabber:server` to
   `jabber:client` for typed parsing (RFC 6120 §4.8.3); only the stanza and
   its direct children are mapped, so a stanza nested in a payload keeps the
   namespace it declares;
2. applies the identity profile to every address the stanza carries
   (`enforce_stanza_addresses`: payload `jid` attributes, mediated
   invitations, `jid-single` and `jid-multi` form fields), the rule C2S
   applies;
3. parses the typed stanza;
4. reads the sender as a `RemotePrincipal`;
5. classifies `to` with the served domains and decides what it reaches
   (next section).

**Refused spellings.** A `to` or `from` that is a valid address but written
in a spelling the profile refuses (an uppercase localpart, a width or
compatibility form) no longer closes the stream. The codec reads it through
RFC 7622 preparation only to compare its domain with the pair and to
address a reply; its domain must still be the stream's, or the stream
closes with `host-unknown` or `invalid-from` as before. The stanza is handed
on as malformed, with both addresses in the canonical form of their
preparation, and is never processed: the router answers it with
`jid-malformed` (type `modify`), from the prepared `to` to the prepared
`from`, with the stanza's own `id` (a message's included). An address
nested in the payload outside the profile is answered the same way. Such a
stanza that is an IQ `result` or `error` gets no reply (RFC 6120 §8.3.1),
but it still ends the request it answers: if it would have completed a
pending request (same sender, recipient and id, and a payload in the
request's namespace), the request is completed with `service-unavailable`
(type `cancel`) from the remote address, generated by this server, so the
requester learns at once instead of at expiry. A stanza that is not a
valid stanza of its kind (an IQ `get` without a payload) is answered with
`bad-request`. Stream errors remain for
an absent, empty or unparseable address and for a domain outside the pair.
A sink that refuses a malformed stanza closes the stream with
`improper-addressing`, which is what a stream without a router still does.

**Remote principal.** `RemotePrincipal` carries the sender's canonical
address, its canonical bare JID (the grant and bootstrap subject), its home
domain (the stream's authenticated remote domain), the stream's
authentication strength and trust-profile kind, and the connection and
configuration generation it arrived under. It is built only from a stanza
on an inbound stream whose remote domain is exactly the address's domain;
it is never a session, an account or a grant. The effect-boundary admission
slice (W5e) consumes it.

## Reachability inventory (G23)

No handler written for client sessions sees a stanza from an S2S stream:
the router is the only entry point, and it dispatches nothing to the
WebSocket namespace handlers, the message and ingress pipeline, the
presence, roster and subscription handlers, MUC, PubSub, upload, push,
extension or call code. What a stanza reaches is decided by
`s2s::route::reach` from its destination and kind alone, with one
exception decided first from the shared occupancy table: a message or
presence from a remote room to the full JID of a local resource that
joined it is delivered to that resource
([Outbound MUC](#outbound-muc-w6b)).

| Destination | IQ `get`/`set` | IQ `result`/`error` | Message | Presence |
| --- | --- | --- | --- | --- |
| Server domain | `disco#info` and `disco#items` answered (below), ping answered with an empty result; anything else `service-unavailable` | Completes the `disco#info` this server sent to verify a remote room's service domain, if it matches one pending (sender, id, payload namespace); otherwise dropped | `service-unavailable` | Ignored |
| Server domain with a resource | `service-unavailable` | Dropped | `service-unavailable` | Ignored |
| Local account, bare JID | `service-unavailable` (D7) | Dropped | `service-unavailable` (D7) | Ignored (D7) |
| Local account, full JID | `service-unavailable` (D7) | Delivered to that resource only if it completes a request that resource sent (D7's first exception) | From the bare JID of a remote room at a verified service the resource's join was sent to, or from one of its occupants once the room has sent the resource its own presence, until the leave is answered or its grace ends: delivered to that resource (D7's second exception, below). Otherwise `service-unavailable` (D7) | From a remote room at a verified service the resource's join was sent to, or has just left: delivered to that resource (D7's second exception). Otherwise ignored (D7) |
| MUC domain: service, rooms, occupants | `service-unavailable` until the inbound MUC slice (W6a) reviews its handlers for remote principals | Dropped | `service-unavailable` | Ignored |
| Spaces PubSub domain | `service-unavailable` until the Spaces slice (W7a) reviews its handlers | Dropped | `service-unavailable` | Ignored |
| HTTP upload, push, community, extensions, call mixer | `service-unavailable` (D10: not enabled) | Dropped | `service-unavailable` | Ignored |

In every row an error stanza is never answered, and presence whose
addresses fall outside the identity profile, or that is not valid
presence, is dropped unanswered and counted
(`waddle.s2s.route.unusable_presence`): an error presence would reach a
room as its occupant's answer and remove the occupant. A remote peer can only
open a stream to a domain in the S2S configuration's local domains, which
startup restricts to the account, MUC and Spaces domains, so the last row is
unreachable at the stream level and refused again here.

Server-domain discovery for remote askers answers `disco#info` with one
identity (`server`/`im`) and the one feature a remote entity can rely on
here, `http://jabber.org/protocol/disco#info` (XEP-0030 §3.1). Further
features are advertised to remote askers only once verification has shown
them (gate G21). `disco#items` lists no item until a service is reachable
by remote senders; a `node` on either query is `item-not-found`.

Besides the reachability table, a stanza from an S2S stream can cause only
these effects:

- a reply queued on the egress queue of the same domain pair; a reply
  that does not fit, or that would open a pair's queue beyond the reply
  share, is dropped;
- one `DELETE` on the correlation table, and for a completed room
  verification one upsert of the service verdict cache;
- a change to the shared occupancy of a remote room the local resource
  holds (below), and the delivery of that room's presence and messages to
  that resource, or of the unavailable presence this server generates when
  the stream to the room's domain fails;
- the sending of a join this replica held until the room's verification
  completed, or the error presence answering it;
- the delivery of a correlated response, or of the error that replaces a
  response this server refused, to the one local resource that sent the
  request. Delivery uses the WebSocket delivery path: on another replica
  through the cluster's existing registered-remote delivery, and for a
  detached resource with stream management into that session's custody,
  from which it is replayed on resumption. A remote entity's response
  passes the recipient's XEP-0191 blocking list, so a response from an
  address the requester blocked after sending is completed and then
  dropped: the requester receives nothing. An error this server generated
  bypasses the blocking list.

Enabling a further service, or a further operation of one, for remote
senders is its own reviewed change to `reach` (or, for room traffic, the
router's `rooms` module), this inventory and the reachability or outbound
MUC suites. MUC and the Spaces PubSub service are in D10's enabled
set; W6a and W7a enable them when their handlers accept a remote principal.

## Account traffic in the interim (D7)

Inbound, as the inventory shows: traffic to a local account is refused as
if the account did not exist (RFC 6121 §8.5.1), except a response that
completes a request the account's resource sent, and traffic from a
remote room at a verified service to the full JID of a local resource that joined it
([Outbound MUC](#outbound-muc-w6b)).

Outbound, a local account's messages and presence for a remote JID are
refused by the router with `service-unavailable` (D7), unless they are for
a remote room the sending resource joins, occupies or leaves; the error
comes back on the sender's own connection. Presence, subscription requests
and responses included, is intercepted before any roster, subscription,
directed-presence or room handling, so no roster state changes. IQ requests
to remote entities are routed, so discovery, ping, PubSub and room
requests (self-ping, XEP-0313 queries, room configuration) keep working.
This outbound refusal is reviewed, not run: account-traffic work (W6c)
replaces it and rewrites its cases before verification. The room exception
is covered by the outbound MUC suites, written and not yet executed.

## Egress

Local stanzas reach the remote route from four places. Three stamp the
bound full JID as `from` first (RFC 6120 §8.1.2.1): client IQ requests and
responses before namespace dispatch and, new in this slice, client
messages and presence addressed to an unserved domain (a subscription
request or response carries the account's bare JID). The fourth, IQs the
interpreter routes, carries the `from` the handler that produced the IQ
set; one without `from` is dropped. A message for an unserved domain never
enters the ingress authority: it is classified before the authoritative
message path.

A client IQ request to an address on the sender's own XEP-0191 blocking
list (a full, bare or domain entry) is not offered to the route at all: it
is answered with `not-acceptable` (type `cancel`) and the `<blocked/>`
condition, from that address, as XEP-0191 §3.3 requires and as a request
to a blocked local address already is. If the blocking list cannot be read,
the request is refused with `internal-server-error`.

`S2sRouter::route` decides without waiting, in this order:

1. no transport attached, or shutting down: `resource-constraint`;
2. a domain that cannot name a peer (IP literal, numeric IPv4 form, single
   label, unpreparable): `remote-server-not-found`; the classifier refuses
   these first, and the router refuses them again by the identity profile's
   classification;
3. a served or local domain is never routed to a peer (RFC 6120 §10.5);
4. a domain peer admission does not admit (D1, denylisted parents
   included): `remote-server-not-found`, so admission is not disclosed;
5. a sender outside the S2S local domains: `service-unavailable`;
6. by kind: an IQ request only from a bound resource of a local account,
   with a non-empty id of at most 256 bytes, sender and recipient
   addresses of at most 1023 bytes each, and not already pending for the
   same recipient (`bad-request` otherwise), within the bounds on pending
   requests for the replica, the remote domain and the resource
   (`resource-constraint`); an IQ response is queued and sent only if it
   completes a pending remote request; messages and presence are refused
   (D7) unless a bound resource of a local account sends them to a remote
   room it joins, occupies or leaves, within the bounds on joined rooms
   (`resource-constraint`);
7. the stanza, with server-minted credentials removed, goes on the bounded
   queue of its directional domain pair; a full queue, or too many pairs,
   is `resource-constraint`.

Step 2 repeats a check the classifier makes, so the route never relies on
its caller having classified the destination.

One worker per pair takes the queue in order. A response is first matched
against the request it answers and dropped, before anything is dialled, if
it completes none; one that completes a request and then finds no stream
is lost, as it would be anyway. The worker dials the pair's stream on
demand through the connection manager, which applies admission, backoff,
caps and deadlines; registers a request in the correlation table before
the first byte; and hands the stanza to the stream. A stream holds one stanza at a time and reports
whether its bytes were written to the TLS session within the write
deadline, so a slow peer pushes back on the worker, the queue fills and new
stanzas are refused. The stream writes the stanza in the `jabber:server`
content namespace, refuses one whose `to` and `from` are not at its pair,
and writes both addresses with the domain in U-labels, as RFC 7622 §3.2.1
asks of an address presented to a peer and as stream headers already are.

If the stream cannot be established, the worker answers the request with
`remote-server-not-found` when no route exists (admission, resolution) and
`remote-server-timeout` otherwise (RFC 6120 §10.4.3), and answers everything
queued behind it the same way. A peer that refuses the stream with
`conflict` because it already holds one for the pair, typically another
replica's, ends the attempt like a local `conflict`: `remote-server-timeout`
(type `wait`, so the requester may retry) and no backoff for the domain.
An idle queue ends after a minute; shutdown answers what is still queued as
`remote-server-timeout`.

**Merging with the dialback slice (W4d).** W4d adds
`ConnectError::RateLimited`. `Failure::from_connect` in `route/router.rs`
matches `ConnectError` exhaustively, so the merge must add the arm there:
`RateLimited` maps to `Failure::Timeout` (`remote-server-timeout`, type
`wait`), and it is not a peer failure, so it causes no backoff.

The per-peer, per-resource and reply bounds are shares of the global ones,
so that one remote domain, one local resource or remote senders alone
cannot take all of a replica's capacity.

| Bound | Default |
| --- | --- |
| Stanzas queued per directional pair | 64 |
| Directional pairs with a queue | 64 |
| Pairs a reply to a remote sender may open a queue for | 32 |
| Requests pending on one replica | 4,096 |
| Requests pending to one remote domain on one replica | 1,024 |
| Requests pending for one local resource | 16 (below the per-pair queue) |
| Request lifetime | 120 s |
| Responses being correlated at once | 64 |
| Waiting for one stanza to be taken and written | 10 s |
| Idle queue lifetime | 60 s |
| Sweep interval: expiry, release of requests completed on another replica, liveness | 5 s |
| Pruning of an expired row whose replica stopped recording that it is alive | 600 s after expiry and after the replica's last record |
| Sender and recipient address of a correlated request | 1,023 bytes each |
| Id of a correlated request | 256 bytes |

## IQ correlation (G8)

**Decision: a durable correlation table, not the cluster relay.** A
request is sent on the replica that holds the requester's connection, but
the peer's response arrives on whichever replica the peer's inbound stream
reaches, and a peer can send the same response on several streams. At-most-
once completion therefore needs one linearisation point shared by every
replica. Two were considered:

- **Cluster relay.** The receiving replica would forward an uncorrelated
  response to the replica that owns the request, which would complete it
  in memory. That needs a new relay message, and so a new ordered relay
  protocol version, an owner lookup per response, and handling for an
  owner that restarted or a requester that moved; and it does not exist
  without clustering.
- **Durable table** (chosen). The replica that writes a request inserts one
  row in `s2s_iq_correlations` in the global database; whichever replica
  receives the response completes it with one `DELETE` on the key, which
  removes the row at most once in both PostgreSQL and SQLite. Failure
  before the first byte and expiry use the same `DELETE`, and only the
  caller whose `DELETE` removed the row acts, so a request receives at most
  one of its response, its failure error and its expiry error. The
  completing replica delivers the response through the existing delivery
  path to the requester's resource, which reaches another replica through
  the cluster's existing registered-remote delivery. The sending replica
  reads its own rows at each sweep and releases, without answering, every
  request it registered whose row is gone, so its bounds are freed within
  one sweep interval of a completion elsewhere. No relay type changes, and
  a single-process deployment needs nothing more.

The cost is one insert and one delete per request to a remote entity, and
per replica one indexed read of its own rows and one liveness write per
sweep, on the global database. A registration replaces an expired request
with the same key and inserts the new one in one transaction. The key is
bounded in bytes (addresses of at most 1023 bytes, an id of at most 256,
checked before insert and by the table's constraints), so that its primary
key entry stays under 2400 bytes, below PostgreSQL's B-tree limit of 2704
bytes with the default 8 kB page; a longer key is refused with a typed
error and the request answered with `bad-request`. Expiry compares wall-clock milliseconds across
replicas, so clock skew between replicas shifts expiry by the skew; the
120-second lifetime is far larger than the skew the cluster tolerates
elsewhere.

**Matching.** The key is the direction, the local address, the remote
address and the id. For a local resource's request, the local address is
its bound full JID and the remote address is the request's `to` exactly, so
a response matches only when it comes from the request's recipient,
addressed to that resource, with the same id (RFC 6120 §8.2.3). A result
carrying a payload completes the request only when the payload is in the
request's payload namespace; a result without a payload, or an error,
completes any request. Another entity of the same domain, another resource
of the same account, a different id, a payload in another namespace, a
response after expiry and a replay all complete nothing and are dropped.
The request's type (`get` or `set`) is stored but not matched: RFC 6120
§8.2.3 answers both with the same response types, so a response carries
nothing to match it against.

**Expiry.** Each replica keeps its own requests in memory to apply the
bounds and to expire them. Every five seconds it expires its requests whose
lifetime has passed; one it removes was never answered, so the requester
receives `remote-server-timeout` (type `wait`). In the same sweep it
releases the requests completed on another replica and records that it is
alive in `s2s_iq_correlation_owners`. An expired row is pruned only when
its replica has not recorded that it is alive for ten minutes, so pruning
never takes a request from a replica that would still answer it; a
replica whose database writes fail for that long may find its expired
requests pruned and then releases them without an answer. A live replica
also removes its own rows that expired more than ten minutes ago: a row
whose registration committed although the database reported a failure is
held by no reservation in memory, so nothing else would remove it.
Liveness is judged by the same wall-clock milliseconds, so a replica whose
clock lags the pruning replica's by more than ten minutes is taken for
stopped, and its expired requests are pruned before it answers them.

**Inbound requests.** The table's `inbound` direction holds a remote
entity's request to a local entity, completed by the local response. No
inbound request reaches a local entity in this slice, so it is never
written, and every local IQ response offered to the route is dropped. The
slices that let remote requests reach local handlers (W6a, W6c) register
them.

## Federated handoff contract (G8)

**Custody.** `route` returning `Accepted` means only that the replica's
queue for the pair holds the stanza, in memory. The queue is not durable:
a crash or restart loses what it holds without an error to the sender.
`Accepted` is never reported to a client as success, and nothing is
fabricated on a peer's behalf. A written stanza has reached the TLS
session; neither a socket write, nor queue insertion, nor the peer taking
it proves delivery to the recipient.

**Retry identity.** A stanza is written at most once. The worker offers it
to a second stream only when the first provably never took it: the stream
had already been replaced or was closing, and returned it unwritten. The
second offer carries the identical stanza (same `id`, `to`, `from` and
payload) for the same pair. A stanza a stream took and whose write did not
complete in time, or whose connection ended mid-write, has an unknown
outcome and is never sent again; a request in that state stays pending
until its response or its expiry. Retries never expand the recipient set:
each queued stanza has exactly one `to`, fixed when the router accepted it,
and the egress path neither fans out nor re-resolves recipients.

**Ordering scope.** Stanzas from one replica for one directional domain
pair are written in the order the router accepted them: one worker, one
stream, one stanza at a time. There is no ordering across pairs, across
replicas, or across a stream replacement for a stanza whose outcome is
unknown, and no global or exactly-once delivery.

**Error responsibility.**

- A stanza `route` refuses is answered at once by the caller: the error for
  an IQ request (`offer_iq`), or for a message or presence (`offer_stanza`),
  with the refusal's condition. Errors and IQ responses are never answered
  (RFC 6120 §8.3.1). A client request to an address on the sender's own
  blocking list never reaches `route` and is answered with
  `not-acceptable`.
- A message or presence for a remote room that the router accepted and
  could not write, because no stream could be established or the stream
  provably did not take it, is answered through the local delivery path
  with `remote-server-not-found` or `remote-server-timeout`, and a join
  that never left is forgotten. One whose write outcome is unknown is not
  answered.
- A request the router accepted and could not write is answered by the
  router through the local delivery path: `remote-server-not-found`,
  `remote-server-timeout`, `bad-request` for an id already pending on
  another replica, or `internal-server-error` when the table or the codec
  failed.
- A request with an unknown write outcome, or without a response, is
  answered with `remote-server-timeout` at expiry, unless a response
  completed it first.
- A response this server refuses to process because an address it
  carries is outside the identity profile completes its request with
  `service-unavailable`.
- Every error the router generates for a local requester names the
  remote address as `from` but is delivered outside the requester's
  blocking list, since the remote entity did not send it.
- An error from the remote server about delivery to its recipient (RFC
  6120 §10.4.3, second paragraph) arrives as an IQ error and completes the
  request like any response.
- A reply the router generates for a remote sender is dropped on any
  failure; nothing answers it.

**Reconciliation with the ingress effect acceptance contract.** That
contract freezes a direct message's targets and child identities before it
is acknowledged, and lets an unknown started send retry after its deadline.
It is unchanged: a message for an unserved domain is classified before the
ingress authority sees it, so no frozen target is ever remote. Outbound MUC
keeps it so: a groupchat or private message to a remote room goes to the
router from the same classification and never enters the ingress
authority, so it is not archived, carbon-copied or projected into the inbox
by the sender's server (the room archives groupchat; XEP-0313 §5), and
`Accepted` stays transport custody only. When account traffic (W6c) lets an
ingress-accepted message go to a remote JID, that slice must record S2S
egress as transport custody, never delivery, and may retry only a stanza
the router reports as never taken; a retry after an unknown outcome would
contradict the retry identity above.

## Outbound MUC (W6b)

Local resources join rooms on remote domains (XEP-0045, pinned at
4132c531487fe9450ae90b905b8df950d0d478b2), as D7's room exception allows in
both directions. The implementation is in `crates/waddle-server/src/s2s/route/`:
`rooms.rs` (what a stanza does to an occupancy), `occupancy.rs` (the shared
table), `room_delivery.rs` (the inbound queues) and their use in
`router.rs`; the stream-end notification in `s2s/conn/sink.rs` and
`s2s/conn/session.rs`; the leave when a resource ends in
`server/routes/websocket/cleanup.rs`; and the adoption on resume in
`server/routes/websocket/stream_management/registration.rs`.

**What makes a remote JID a room.** A remote JID is a room for a local
resource only when two things hold:

- the resource sent it a join: presence without a type, from a bound
  resource of a local account, to an occupant JID (a bare JID with a
  localpart, and a nickname as resource), carrying the
  `<x xmlns='http://jabber.org/protocol/muc'/>` element (XEP-0045 §7.2.1);
- the JID's domain answered this server's XEP-0030 `disco#info` as a
  multi-user chat service, with the `conference`/`text` identity or the
  `http://jabber.org/protocol/muc` feature (XEP-0045 §6.2).

Every bare JID with a localpart at such a service is a room, or one that a
join creates (§10.1). The service is asked rather than the room because a
room that does not exist yet answers its own `disco#info` with
`item-not-found` (§6.4), so asking the room would refuse every room
creation. An account's domain answers as a server (`server`/`im`), so a
join to an account is refused. The request goes from the local domain (the
server, not the client) to the service's domain JID, which no client can
answer. It is registered in the correlation table like a client's
request, with the server domain as local address and a generated id, so
it completes at most once, on whichever replica its response arrives, and
expires after 30 seconds. Its response reaches the router as an IQ result
or error to the server domain (the reachability inventory). The replica
that completes it caches the verdict per service domain in
`s2s_remote_room_verdicts` for every replica: "service" for an hour, "not
a service" for five minutes. A result without the identity and feature,
or an error other than a temporary one, is "not a service"; an error of
type `wait` or with a `remote-server-*` condition, an answer the identity
profile refuses, and no answer at all are not cached.

The occupancy is recorded, within its bounds, before anything is asked.
While the service is not known, the join is held in memory on the replica
that routed it, and nothing reaches the JID. When the verdict arrives (on
this replica at once; from another replica's completion at the next
sweep, at most five seconds later), the join is sent; for a domain that
is not a service the client receives an error presence `item-not-found`
from the occupant JID, with the join's id and the `muc` child; for a
domain that did not answer, `remote-server-timeout`; for a domain that
cannot be reached, the route's own error. A join to a domain cached as not
a service is refused at once with `item-not-found`. A join that may not be
held (more than 256 services being asked on the replica) is refused with
`resource-constraint`. Because nothing reaches an unverified JID, a
join's `<status/>` text never does either.

**Shared occupancy.** Each occupancy is one row of
`s2s_remote_room_occupancies` in the global database (migration 2004):
the local full JID, the room's bare JID, the nickname, the phase, the
affiliation the room last stated, the join it waits on and that join's
presence id, leaves whose answer is still to come, the replica whose
router last routed the resource's room stanzas (`owner_id`, the router's
correlation owner), a deadline, and a revision. Every write after the
insert is a compare-and-set on the revision, retried a few times against
concurrent writers; a removal also names the join attempt the row was
read with, since a row removed and inserted again starts its revisions
again but always names a new attempt. Any replica admits a room's traffic from the table,
whichever replica the room's server dials, and delivers it through the
local delivery path, which reaches the occupant's session on another
replica through the cluster's registered-remote delivery. A session that
resumes through the durable stream-management authority adopts its rows
for its new replica.

The phases are:

- *verifying*: the join is held while the room's domain is asked whether
  it is a multi-user chat service; nothing is sent to the room and nothing
  from it is admitted;
- *joining*: the join was sent; the room has not yet sent the occupant's
  own presence;
- *joined*: the room sent the occupant's own available presence;
- *leaving*: the resource sent unavailable presence; the room's answer and
  what it sent before reading the leave are admitted for 30 seconds.

A join for a room the resource has joined, such as a client
resynchronising after it reconnected under the same full JID, is sent and
changes nothing, so the room's traffic keeps arriving while the answer is
on its way. A join while joining is sent and waits for the room anew. A
join while leaving is sent, and the leave's answer still to come, the
occupant's own unavailable presence, is absorbed while the leave's grace
lasts instead of ending the new join. A join while verifying replaces the
join held.

**Outbound.** For a room with an occupancy, the router lets these leave:
messages of every type to the room or to one of its occupants (groupchat,
private messages, mediated invitations and voice requests) once joined;
presence without a type to an occupant JID (a status update or a nickname
change, §7.6 and §7.7) while joining or joined; error presence while
joining, joined or leaving; and unavailable presence (§7.14) in any phase.
A leave while verifying sends nothing and forgets the occupancy. Joins,
leaves and their occupancy changes are decided against the table, the
change written with the stanza's place on the pair's queue already
reserved, and the stanza queued once the write succeeded. Everything else
is account traffic and refused with `service-unavailable` (D7): messages
and presence to any other remote JID, presence to a room's bare JID,
subscriptions and probes to anything, room traffic from a resource of the
account that did not join, messages before the room has seated the
resource, and room traffic after the resource left.

**Inbound.** A message or presence from a remote JID to a local full JID
is not decided on the stream's task: it is queued for one of four workers,
each serving its own share of rooms, which decides it against the table,
applies what it does to the occupancy, and delivers it if it is admitted
or refuses it as the reachability table says otherwise. Admitted:

- while joining, joined or within the leave's grace: presence carrying the
  `muc#user` element, or an error, and messages from the room's bare JID,
  such as a challenge the room sends before seating the occupant;
- once joined, and within the leave's grace: messages from the room's
  occupants, including the subject, history, private messages, MUC user
  statuses and XEP-0313 results.

Nothing is admitted while verifying: a client cannot open a conversation
with an account by "joining" it, since the account's domain answers as a
server, not a multi-user chat service, and the join is never sent. The room's own presence
moves the occupancy on (status 110, or the occupant JID with the current
nickname): available presence completes the join and records the nickname
the room assigned (§7.2.2) and the affiliation it stated; unavailable
presence with status 303 records the new nickname (§7.6); any other own
unavailable presence answers a leave still pending from before a rejoin,
or else ends the occupancy (left, kicked, banned, removed, destroyed); an
error presence ends a join in progress (§7.2.5 to §7.2.11). Admitted
traffic reaches that full JID only, through the local delivery path with
the recipient's XEP-0191 blocking list applied; a bare-JID address is
never room traffic. IQ requests from a room or its occupants are refused
as W5a refuses every IQ request to a local account, and IQ responses are
correlated as before. Traffic from a room the resource never joined, or
for a resource that did not join it, takes the reachability table's D7
refusal.

Each worker serves its rooms in turn, one stanza each, and a room's
stanzas in the order they arrived, so an occupant sees a room's presence
and messages in the room's order, and the room's own presence moves the
occupancy on before its next stanza is decided. A room may have at most
64 stanzas waiting and a worker 256; a stanza beyond either is dropped and
counted (`waddle.s2s.route.room_traffic.dropped`). A delivery that takes
more than five seconds is abandoned and counted
(`waddle.s2s.route.room_traffic.delivery_timeouts`).

**Carbons, archive and inbox.** A private message that a local resource
receives from a remote room occupant takes the recipient's peer-stanza
pass, as a chat message from any remote entity does: the recipient's
XEP-0191 blocking list applies, a XEP-0359 stanza-id is stamped, and it is
archived in the recipient's archive and projected into the inbox with an
unread increment, both keyed by the room's bare JID. It is not
carbon-copied to the recipient's other resources (XEP-0280 §6.1): the room
already delivers it to every session joined under that nickname. The
server recognises such a message by its `muc#user` `<x/>` element and an
occupant `from` outside the account domain; a room that omits the element,
which XEP-0045 §7.5 only recommends, has its private messages copied.
Groupchat messages and MUC presence from the room are delivered to the
occupant's full JID; none is archived in the account's archive,
carbon-copied or projected into the inbox.

A message a local resource sends to a remote room goes to the S2S router
without entering the ingress authority, so it is not archived by the
sender's server (the room archives groupchat; XEP-0313 §5). Once the
router accepts it, an eligible message is copied as a `<sent/>` carbon to
the sender's other Carbons-enabled resources, including resources not
joined to the room (the multi-session nickname limit in the §6.1 note is
a recommendation). Eligible messages are a private message to an
occupant, and one carrying a mediated or XEP-0249 invitation. Groupchat,
messages carrying `<private/>` or a full-JID `<no-copy/>`, and anything
the router refuses are not copied.

**Self-ping (XEP-0410).** A self-ping is an IQ request to the occupant's
own JID, so it is routed and correlated like any request, whether or not
an occupancy exists, and the client receives the room's answer as the room
sent it: a result while joined, `not-acceptable` once the room no longer
seats the occupant (the occupancy then stays until the client joins again
or leaves). A room without the self-ping optimisation reflects the ping
to the occupant (§3.2); the reflection is an IQ request to a local account,
which this server refuses with `service-unavailable`, and the room relays
that answer, which XEP-0410 reads as "joined, the client does not
implement ping". If the room cannot be reached, the client receives
`remote-server-not-found` or `remote-server-timeout`, which XEP-0410
treats as a timeout.

**XEP-0313 queries.** A query to a room is an IQ request: routed,
correlated and answered with its `<fin/>`. Its result messages come from
the room's bare JID and are admitted as room traffic while the resource
is joining or joined; a query to a room it has not joined receives its
`<fin/>` but none of the results. The `<fin/>` is an IQ response and is
delivered outside the room's queue, so it can reach the client before the
last results.

**When a join is not answered.** A join the room has not answered within
60 seconds of being sent ends: any replica's sweep removes the occupancy
and the client receives an error presence `remote-server-timeout` from the
occupant JID with the join's id. A join held for verification on a
replica that stopped ends the same way after twice the verification
timeout. A leave's grace ends silently.

**When the stream fails.** The connection manager tells its stanza sink
when an established stream ends, and how:

- *Lost*: a transport or TLS error (`io`, `tls`); end of file, the idle
  timeout or the maximum age while the peer was in the middle of a
  first-level element; a write that did not complete within the write
  deadline; a first-level element the peer left unfinished past the
  incomplete-element deadline; a stanza the router could not queue within
  its deadline (see Inbound); a stream error this server sent
  (`protocol`) or the peer sent other than `conflict` and
  `connection-timeout` (`peer-error`, which includes the peer's
  `system-shutdown`); a domain peer admission no longer admits
  (`admission`); or a certificate or dialback failure;
- *Closed*: either side sent its closing stream tag; the peer ended the
  stream with `connection-timeout`; end of file between elements; the
  idle timeout with nothing unfinished (`timeout`); the connection's
  maximum age with nothing unfinished (`expired`); eviction; replacement
  by a newer stream for the pair (`conflict`); or this server shutting
  down. Peers close idle streams with `connection-timeout` or by dropping
  the connection, and a stream idle long enough to be closed carries
  nothing in flight.

When an inbound stream from a remote domain to a local domain is lost,
every occupancy of a resource of that local domain in a room at that
remote domain ends, whichever replica routed it: the stream carried the
rooms' traffic for all of them. When an outbound stream is lost, the
occupancies this replica routes end: the stream carried their stanzas.
Each end is queued on the room's worker behind the traffic already
queued, exempt from the queue's bounds, and then removes the occupancy,
unless a later join replaced it, and delivers the occupant's own
unavailable presence from the room with statuses 110 and 333 and its
affiliation, role `none` (XEP-0045 §11.1, without 307). Stanzas either way
may have been lost and the room removes an occupant it cannot reach, so
the client is told and joins again. A closed stream changes nothing:
membership of a remote room does not depend on a stream, the next stanza
opens a new one, and peers close idle streams, as this server does. A
stream that cannot be established for a stanza is not a loss either: the
stanza's sender is answered with the error, and a join that never left is
forgotten.

**When the resource ends.** When a resource ends for good (its connection
closed without stream management, its detached session expired or was
invalidated, stream management could not detach it, or terminal recovery
found nothing to detach), it leaves every room it joins or occupies,
whichever replica routed it: the router sends unavailable presence from
the full JID to its occupant JID in each, and removes the occupancies. A
join still held for verification is dropped. The WebSocket cleanup does
this only when no replacement holds the same full JID: a remote room knows
its occupant only by that full JID, so a replacement inherits the
occupancies, and a leave sent for the old session could reach the room
after the replacement's own join and remove it. A detached session that
may still resume keeps its rooms. A client's own broadcast unavailable
presence does not leave rooms, as it does not leave local rooms either.
Each occupancy is removed only while it still names the join and the
replica it named when the leave began: one that a later join replaced,
or that a session resumed elsewhere adopted, in the meantime belongs to a
session that lives on and is kept.

**When a replica stops.** Each router records that it is alive in
`s2s_iq_correlation_owners` at the start of every sweep, before the sweep
answers anything, and every delivery a sweep makes waits at most the
delivery timeout, so a slow local delivery does not let other replicas
take this one for stopped. An occupancy not written for
ten minutes whose owner has not recorded that it is alive for as long is
ended by another replica's sweep: the room is sent the resource's leave
and the resource, if its session lives on elsewhere, receives the status
333 presence and joins again. A session that resumed elsewhere through
the durable authority has adopted its rows and is not affected.

**Bounds.**

| Bound | Default |
| --- | --- |
| Remote room occupancies of one local full JID, in every phase | 128 |
| Remote room occupancies of one local account | 512 |
| Remote room occupancies of the deployment | 65,536 |
| Remote service domains one replica is asking at once | 256 |
| How long a join is held for verification | 30 s |
| How long a sent join waits for the room's own presence | 60 s |
| How long a room left keeps admitting the room's answer and earlier traffic | 30 s |
| How long a "service" verdict is cached | 1 h |
| How long a "not a service" verdict is cached | 5 min |
| Room traffic waiting for decision on one replica (four workers of 256) | 1,024 |
| Room traffic waiting from one room | 64 |
| One delivery of room traffic | 5 s |

A join beyond the occupancy bounds is refused with `resource-constraint`.
The bounds are counted and the row inserted in one transaction. On SQLite
it holds the write lock, so every bound holds exactly. On PostgreSQL the
transaction runs at READ COMMITTED and first takes a transaction advisory
lock keyed on the account (`pg_advisory_xact_lock` of
`hashtextextended(account, seed)`), so the joins of one account, and so
of each of its resources, are counted and inserted one at a time and the
per-resource and per-account bounds hold exactly. Joins of different
accounts are not serialised, so the deployment's total can be exceeded
briefly, by at most the joins of other accounts in flight at the same
moment. No join counts the whole table: the per-resource and per-account
counts read the primary key and the account index, and every count stops
at its bound, so the total reads at most as many rows as the deployment
bound (65,536 by default). The verdict cache holds one row per
service domain asked within the TTLs and is pruned with the correlation
table.

## Behaviour with S2S off

Outbound MUC changes nothing while S2S is disabled: the remote route still
refuses a remote room's join with `remote-server-not-found`, and the leave
when a resource ends, like the adoption when a session resumes, is a
registry lookup and a call that does nothing. Received private messages
carrying the `muc#user` element from a full JID outside the account
domain are no longer received-carbon-copied (XEP-0280 §6.1), whether or
not S2S is on.

These change behaviour while S2S is disabled, and belong to the rollout
runbook (G24):

- A client message to an unserved domain is now answered with a message
  error `remote-server-not-found` (type `cancel`), from the addressed JID,
  without echoing the body, and never enters the ingress authority.
  Before, as read from the code at the branch point (not executed), a
  bound client's `chat` or `normal` message with a body was
  accepted by the ingress authority and committed: it was archived in the
  sender's MAM archive, projected into the sender's inbox with the remote
  JID as peer, and carbon-copied to the sender's other carbons-enabled
  resources; then the router found no local recipient for a non-local
  domain and dropped it with no error, no offline row and no receipt, so
  the sender saw only the stream-management acknowledgement. A `headline`
  without `<store/>` was dropped with nothing recorded beyond the ingress
  commit; a `groupchat` message was answered with `item-not-found` ("room
  not found"); an `error` message was ignored. Clients that showed a sent
  message as delivered on the strength of the archive or the acknowledgement
  now receive an error instead, and nothing is archived.
- Client presence to an unserved domain is answered with a presence error
  `remote-server-not-found`. Before, a subscription request to a remote JID
  was answered with `unsubscribed` from the contact, other subscription
  presence was ignored, and directed presence, a remote room join
  included, went to the directed-presence handler, which found no
  recipient and dropped it without an error.
- A client IQ request to a remote address on the sender's blocking list
  is answered with `not-acceptable` and `<blocked/>` instead of
  `remote-server-not-found`.
- A message carrying a direct-message pin marker (`urn:waddle:pin:0`)
  for a remote peer was refused by the DM pin handler with `forbidden`. It
  now goes to the remote route first, so it is answered with
  `remote-server-not-found` like any other message for an unserved domain,
  and no pin is recorded.
- Migration 2003 creates `s2s_iq_correlations` and
  `s2s_iq_correlation_owners` on both engines, and the owner phase grants
  the runtime `SELECT, INSERT, DELETE` on the first and `SELECT, INSERT,
  UPDATE, DELETE` on the second. Both stay empty while S2S is off.
- Migration 2004 creates `s2s_remote_room_occupancies` and
  `s2s_remote_room_verdicts` on both engines, and the owner phase grants
  the runtime `SELECT, INSERT, UPDATE, DELETE` on both. Both stay empty
  while S2S is off. Like the 2003 tables they belong to no restricted
  same-pool runtime store, so `runtime_store_acl.rs` verifies nothing
  further for them.

## Restrictions and risks

- **Cross-replica latency of a held join.** A join waits for its room's
  verification on the replica that routed it. When the `disco#info`
  response reaches another replica, the join is sent at the routing
  replica's next sweep, up to five seconds later.
- **Bounds under concurrency.** On PostgreSQL the occupancy bounds can be
  exceeded by the joins that commit at the same moment. Verifications are
  bounded per replica (256 service domains), not per account, so one
  account joining rooms on many unknown domains at once can hold all of
  them and delay other joins with `resource-constraint` for up to the
  verification timeout.
- **A cancelled offer.** The WebSocket path waits at most two seconds for
  the route. A join or leave whose occupancy was written but whose route
  call was cancelled before its stanza was queued leaves the occupancy
  ahead of the room: a join ends at its deadline with
  `remote-server-timeout`; a leave ends the occupancy at its grace while
  the room still seats the resource, which joins again to resynchronise.
- **Stopped replicas and quiet rooms.** An occupancy routed by a replica
  that stopped, not written for ten minutes, is ended with a leave and the
  status 333 presence, unless the session resumed through the durable
  authority elsewhere. A session that kept its connection on a live
  replica is never affected. A replica whose liveness writes fail for ten
  minutes has its occupancies ended this way while it still serves them.
- **Verdicts follow the remote server's own answer.** A remote server that
  answers `disco#info` for a domain with the conference identity, wrongly
  or deliberately, has every JID there treated as a room: joins are sent
  and their traffic admitted. A server that hosts accounts and rooms on one
  domain and advertises it as a conference service has its accounts
  joinable the same way. Verification shows that the remote server
  presents the domain as a multi-user chat service, not that a JID there
  is a room.
- **Two replicas dialling one peer.** A peer that keeps one stream per
  directional pair, as Waddle does, refuses a second replica's new stream
  with `conflict` within its replacement interval. Room stanzas a second
  replica sends in that window fail like any of its stanzas (see
  "Several replicas, one stream per pair"): a join or message is answered
  with `remote-server-timeout`, and the leave sent when a resource ends is
  dropped, so the room keeps the occupant until it notices otherwise.
- **Private messages without the `muc#user` element.** XEP-0045 §7.5 only
  recommends the element; a remote room that omits it has its private
  messages received-carbon-copied, as other chat messages are.
- **Room traffic is not checked against the sender's blocking list.**
  Messages and presence for a remote room leave without the XEP-0191
  check client IQ requests get; inbound room traffic does pass the
  recipient's list.
- **An address outside the identity profile in room presence.** A room
  that names an occupant's real JID (`<item jid='…'/>`) in a spelling the
  identity profile refuses has its presence dropped unanswered (gate
  G10), so the occupant does not see it; nothing is sent back to the room.
  This is visible only with third-party peers.
- **A room's self-ping reflection is refused.** Until IQ requests from
  remote entities reach local clients (W6c), a reflected self-ping is
  answered `service-unavailable` by this server, not by the client.

- **Several replicas, one stream per pair.** Each replica dials its own
  outbound stream to a peer, and the connection manager keeps one stream
  per directional pair per process. A peer that also keeps one stream per
  pair, as Waddle does with a replacement interval of 30 s by default,
  alternates between the replicas when both send to it:
  - while one replica's stream is younger than the peer's replacement
    interval, the peer refuses the other replica's new stream with
    `conflict`; that replica answers the requests waiting on the dial, and
    those queued behind them, with `remote-server-timeout` (type `wait`),
    and does not back the domain off, so its next stanza dials again;
  - once the stream is older than the interval, the other replica's new
    stream replaces it, and the peer closes the old one with `conflict`; a
    stanza the old stream had not taken goes to a new stream, which within
    the interval is refused in turn, and one it had taken has an unknown
    outcome and is answered at expiry.
  So with traffic from two replicas, each replica's requests to the peer
  can fail for up to one replacement interval at a time. Responses are
  unaffected: they arrive on the peer's own stream, on whichever replica it
  reaches, and the shared table completes them there. Verification (steps
  6 and 8) runs two replicas against a peer; the remedy, if needed, is to
  route a pair's egress through one replica.
- **No durable egress.** A crash loses queued stanzas without an error;
  the requester's own timeout covers it.
- **Unknown outcomes** are answered only at expiry.
- **Expiry uses `remote-server-timeout`,** the closest RFC 6120 condition
  for a remote entity that did not answer.
- **Ping is answered but not advertised.** The server domain answers an
  XEP-0199 ping from a remote entity, but its `disco#info` for remote
  askers does not list `urn:xmpp:ping`. XEP-0199 §5 (pinned at
  4132c531487fe9450ae90b905b8df950d0d478b2) says an entity that supports
  the protocol MUST report the feature, so this is a known
  non-conformance until gate G21 shows the feature and capability
  advertisement (W11) lists it.
- **Shares of the bounds under open admission (G20).** With D1 in
  `allowlist` mode only listed peers reach the router. Before the default
  moves to `open`, G20's evidence must cover the shares: any admitted
  domain can make the router open a reply queue, so replies are limited
  to 32 of the 64 pair queues, and a pair holds its queue until it has
  been idle for 60 s; a local response that completes nothing still
  occupies its pair's queue until then, although it is never dialled for;
  and one unresponsive remote domain can hold at most 1,024 of a replica's
  4,096 pending requests for their lifetime.
- **Metrics.** The router counts dropped unusable presence, dropped room
  traffic and abandoned room deliveries, with no attribute; its other
  decisions are logged with codes only, never with an address.

## Follow-ups

- Inbound MUC (W6a) and Spaces (W7a): enable their handlers for remote
  principals in `reach`, the inventory and the reachability suite, and list
  them in server-domain `disco#items`.
- Outbound MUC, blocking list: apply the sender's XEP-0191 list to
  messages and presence for remote rooms, with W6c's check for account
  traffic.
- Outbound MUC, broadcast unavailable: decide, with local rooms, whether a
  client's broadcast unavailable presence leaves rooms (RFC 6121 §4.6.3);
  today neither local nor remote rooms are left by it.
- Outbound MUC, carbons of remote room private messages: confirm the
  XEP-0280 §6.1 handling against Prosody and ejabberd rooms (W10), and
  decide whether to recognise a room private message from the occupancy
  table rather than its `muc#user` element, and whether to archive sent
  private messages to remote occupants.
- Outbound MUC, verification bounds: a per-account share of the
  verifications in flight.
- Outbound MUC, IQ requests from rooms: with W6c's inbound request
  correlation, deliver room-reflected self-pings and occupant IQs to the
  occupant instead of refusing them.
- Account traffic (W6c): replace both D7 refusals for reviewed handlers,
  register inbound requests for correlation, and define the ingress receipt
  for a remote target. A message carrying a DM pin marker for a remote peer
  must still be refused before it is routed, since DM pins are local-only:
  today the outbound D7 refusal is what keeps the marker off the wire.
- Effect-boundary admission (W5e): take the `RemotePrincipal` at the
  protected-effect boundary.
- Capability advertisement (W11): any further feature in server-domain
  `disco#info` for remote askers.
- Stanzas the interpreter routes to a remote JID other than IQs, such as a
  MUC mediated invitation to a remote account, are still dropped silently
  by local resource selection; they need the same remote route.
- Router metrics with closed attributes, such as refusals by reason.
- Account traffic (W6c): when messages and presence may go to remote JIDs,
  check the sender's blocking list before offering them, as client IQ
  requests are checked now.
