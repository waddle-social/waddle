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
`server/lifecycle.rs`, and migration 2003 (`s2s_iq_correlations` and
`s2s_iq_correlation_owners`).

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
`s2s::route::reach` from its destination and kind alone:

| Destination | IQ `get`/`set` | IQ `result`/`error` | Message | Presence |
| --- | --- | --- | --- | --- |
| Server domain | `disco#info` and `disco#items` answered (below), ping answered with an empty result; anything else `service-unavailable` | Dropped: this server sends no request of its own to a remote entity | `service-unavailable` | Ignored |
| Server domain with a resource | `service-unavailable` | Dropped | `service-unavailable` | Ignored |
| Local account, bare JID | `service-unavailable` (D7) | Dropped | `service-unavailable` (D7) | Ignored (D7) |
| Local account, full JID | `service-unavailable` (D7) | Delivered to that resource only if it completes a request that resource sent (D7's first exception) | `service-unavailable` (D7) | Ignored (D7) |
| MUC domain: service, rooms, occupants | `service-unavailable` until the inbound MUC slice (W6a) reviews its handlers for remote principals | Dropped | `service-unavailable` | Ignored |
| Spaces PubSub domain | `service-unavailable` until the Spaces slice (W7a) reviews its handlers | Dropped | `service-unavailable` | Ignored |
| HTTP upload, push, community, extensions, call mixer | `service-unavailable` (D10: not enabled) | Dropped | `service-unavailable` | Ignored |

In every row an error stanza is never answered. A remote peer can only
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
- one `DELETE` on the correlation table;
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
senders is its own reviewed change to `reach`, this inventory and the
reachability suite. MUC and the Spaces PubSub service are in D10's enabled
set; W6a and W7a enable them when their handlers accept a remote principal.

## Account traffic in the interim (D7)

Inbound, as the inventory shows: traffic to a local account is refused as
if the account did not exist (RFC 6121 §8.5.1), except a response that
completes a request the account's resource sent. The second exception,
traffic from a remote room to a local occupant, arrives with outbound MUC
(W6b), which has nothing to correlate with until local accounts can join
remote rooms.

Outbound, a local account's messages and presence for a remote JID are
refused by the router with `service-unavailable` (D7); the error comes back
on the sender's own connection. Presence, subscription requests and
responses included, is intercepted before any roster, subscription,
directed-presence or room handling, so no roster state changes. IQ requests
to remote entities are routed, so discovery, ping and PubSub keep working.
This outbound refusal is reviewed, not run: account-traffic work (W6c)
replaces it and rewrites its cases before verification.

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
   (D7);
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
It is unchanged: a message for an unserved domain is refused before the
ingress authority sees it, so no frozen target is ever remote in this
slice. When outbound MUC (W6b) or account traffic (W6c) lets an
ingress-accepted message go to a remote JID, that slice must record S2S
egress as transport custody, never delivery, and may retry only a stanza
the router reports as never taken; a retry after an unknown outcome would
contradict the retry identity above.

## Behaviour with S2S off

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

## Restrictions and risks

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
- **No metrics** are added for the router in this slice; its decisions are
  logged with codes only, never with an address.

## Follow-ups

- Inbound MUC (W6a) and Spaces (W7a): enable their handlers for remote
  principals in `reach`, the inventory and the reachability suite, and list
  them in server-domain `disco#items`.
- Outbound MUC (W6b): messages and presence for a remote room the account
  occupies or is joining, exempt from the outbound D7 refusal; traffic from
  a joined remote room to the occupant's full JID.
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
- Router metrics through the telemetry macros.
- Account traffic (W6c): when messages and presence may go to remote JIDs,
  check the sender's blocking list before offering them, as client IQ
  requests are checked now.
