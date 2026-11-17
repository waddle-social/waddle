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
traffic reaches them ([Outbound MUC](#outbound-muc-w6b)). The account-traffic
slice (W6c) replaces the interim refusal of account traffic, in both
directions, for the handlers it reviewed; its decisions, its inventory of
local-account handlers and the RFC 6121 rules each follows are recorded in
[the account-traffic design](s2s-account-traffic-2047.md) (gate G18).

The inbound MUC slice (W6a) opens the MUC service to remote senders (D10):
accounts at admitted domains discover, ping, join, use and leave rooms of
this deployment and read their archives, each request decided for the
remote principal on the evidence of its stream
([Inbound MUC](#inbound-muc-w6a)).

The Spaces slice (W7a) opens the Spaces PubSub service to remote senders
(D10): accounts at admitted domains discover the Spaces of this deployment
that are open to other servers, read what they list, subscribe to them and
are notified of changes, and change a Space only as D8 allows
([Spaces](#spaces-w7a)).

The MUC fallback slice (W7b) lets a client without Spaces support, local
or remote, reach every child room of a Space by its plain MUC JID: the MUC
service's `disco#items` lists the rooms the asker may reach, remote askers
included, and the `disco#items` of a Space's node lists the rooms the Space
holds that the asker may be told of ([MUC fallback](#muc-fallback-w7b)).

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
- `crates/waddle-server/tests/xep0045_s2s_inbound_muc.rs`: a remote
  occupant joining, seeing and being seen, sending to the room and
  receiving its messages once, leaving, a message without a seat, no room
  creation, the trusted-home rule at issuance and where the seat is taken,
  the refusal of private messages and room administration, the bound on a
  room's remote occupants, removal with status 333 on a delivery error, a
  lost stream and a stanza that cannot be queued, a domain the hosting
  replica no longer admits, the seat table, and on PostgreSQL a managed
  room: entry on a canonical grant, a stranger refused, messages through
  the room's acceptance both ways, and a revoked member removed with
  status 321 and told over S2S;
- `crates/waddle-server/tests/xep0313_s2s_inbound_mam.rs`: a remote
  asker's archive query answered in order before its `<fin/>`, without
  joining an open room, refused for a room that does not exist and for an
  occupant JID, and on PostgreSQL decided on the canonical grant;
- `crates/waddle-server/tests/xep0030_s2s_inbound_muc_discovery.rs`: the
  service and a room described to remote askers, nothing listed, ping, and
  the XEP-0410 self-ping of a remote occupant;
- `crates/waddle-xmpp/tests/xep0178_s2s_negotiation.rs`,
  `s2s_stream_addresses.rs`, `s2s_pre_authentication.rs` and
  `s2s_codec.rs`: stanzas on established streams, U-label addresses, the
  delivery of refused spellings and the content namespace mapping;
- `crates/waddle-server/tests/xep0503_s2s_inbound_spaces.rs`: a federated
  Space listed and described to a remote asker, a Space that is not
  federated and a private Space answered exactly as a missing node in
  every operation, the rooms a Space lists reduced to those open to other
  servers, a read that writes nothing, peer admission at the request, and
  the owner's `federated` field;
- `crates/waddle-server/tests/xep0060_s2s_inbound_spaces_pubsub.rs`: a
  remote account's subscription and its bounds, the request share of one
  account, event notifications to remote subscribers and each condition
  that withholds one, and the changes a remote owner may make only from a
  trusted home;
- `crates/waddle-server/tests/xep0503_s2s_inbound_spaces_host_grant.rs`
  (PostgreSQL): a managed room's bookmark told only to a principal its
  grants admit, and an enrolled scope's Space served to nobody;
- `crates/waddle-server/tests/xep0503_s2s_outbound_spaces_ws.rs`: a local
  account discovering, reading and subscribing to a Space on another
  server's PubSub service, its event notifications, and that the
  subscription grants nothing in the Space's rooms;
- `crates/waddle-server/tests/spaces_privileged_issuance.rs` and
  `admin_spaces_ws.rs`: the privileged relations and roles of Spaces
  written only for served accounts and trusted homes.

The implementation is `crates/waddle-server/src/s2s/route.rs` and `route/`,
the outbound channel of `crates/waddle-server/src/s2s/conn/` (`outbox.rs`
and the session), the codec changes in `crates/waddle-xmpp/src/s2s/`
(`content.rs`, `stream.rs`, the receiving negotiation), `destination.rs`,
the WebSocket call sites (`handlers/unserved_route.rs`, `frame.rs`, the
presence handler, `s2s_delivery.rs`, the blocking check in
`handlers/iq/destination_route.rs`), the wiring in `server/http.rs` and
`server/lifecycle.rs`, migration 2003 (`s2s_iq_correlations` and
`s2s_iq_correlation_owners`) and migration 2004
(`s2s_remote_room_occupancies` and `s2s_remote_room_verdicts`). Inbound MUC
adds `route/local_rooms.rs`, `route/seats.rs` and `route/router/hosting.rs`,
the room host `server/routes/websocket/s2s_rooms.rs`, the delivery hook of
`crates/waddle-xmpp/src/registry/connection_registry.rs` and migration 2007
(`s2s_local_room_seats`). Spaces add `route/local_spaces.rs` and
`route/router/spaces.rs`, the Spaces host
`server/routes/websocket/s2s_spaces.rs` with
`handlers/iq/spaces_remote.rs`, the hook in the PubSub fan-out, the stores
in `db/s2s_spaces.rs` and migrations 2011 (`s2s_federated_spaces`) and 2012
(`s2s_space_subscriptions`).

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
it is never a session, an account or a grant. The host-grant effect
boundary takes the facts of its stream as evidence
(`PrincipalEvidence::of`) and decides its grants from its canonical bare
JID and that evidence under the S2S snapshot the replica runs.

## Reachability inventory (G23)

The router is the only entry point of a stanza from an S2S stream, and it
dispatches nothing to the WebSocket namespace handlers, the roster and
subscription handlers, PubSub, upload, push, extension or call code. What a
stanza reaches is decided by `s2s::route::reach` from its destination, its
kind and what the replica hosts (`Reachable`), with one exception decided
first from the shared occupancy table: a message or presence from a remote
room to the full JID of a local resource that joined it is delivered to
that resource ([Outbound MUC](#outbound-muc-w6b)). Account
traffic goes to the account side (`LocalAccounts`), the one typed entry
the WebSocket layer implements for remote senders' traffic to local
accounts, which startup attaches to the router; a router without one
refuses it as the interim rule below says.

The MUC service and the Spaces PubSub service are the services whose
handlers a remote stanza reaches. The Spaces service is reached only on a
replica whose router has the Spaces host attached (`S2sRouter::attach_spaces`,
which startup calls when S2S is enabled), for five operations
(`SpaceOperation`), each an IQ request: `disco#info`, `disco#items`, ping, a
XEP-0060 request in the `pubsub` namespace and one in the `pubsub#owner`
namespace. Each is decided by the host for the remote principal on the
evidence of its stream ([Spaces](#spaces-w7a)). The MUC service is reached
only on a replica whose router has the room host attached
(`S2sRouter::attach_rooms`, which startup calls when S2S is enabled). Nine
operations reach the host (`RoomOperation`): `disco#info`, `disco#items`,
ping, a XEP-0313 query, presence, a message, a `muc#admin` request, a
`muc#owner` request and a XEP-0425 moderation request. Each is decided
there for the remote principal on the evidence of its stream, and reaching
the host admits nothing ([Inbound MUC](#inbound-muc-w6a)). On a router
without the host, the whole service is refused as it was before.

| Destination | IQ `get`/`set` | IQ `result`/`error` | Message | Presence |
| --- | --- | --- | --- | --- |
| Server domain | `disco#info` and `disco#items` answered (below), ping answered with an empty result; anything else `service-unavailable` | Completes the `disco#info` this server sent to verify a remote room's service domain, if it matches one pending (sender, id, payload namespace); otherwise dropped | `service-unavailable` | Ignored |
| Server domain with a resource | `service-unavailable` | Dropped | `service-unavailable` | Ignored |
| Local account, bare JID | `service-unavailable`, answered on the account's behalf for no namespace (G18) | Dropped | The account side (G18) | The account side (G18) |
| Local account, full JID | The account side admits it for a requester the account shares presence with, or a remote room the resource occupies or is joining: registered as an `inbound` correlation and delivered to that resource; otherwise `service-unavailable` (G18) | Delivered to that resource only if it completes a request that resource sent (D7's first exception) | From the bare JID of a remote room at a verified service the resource's join was sent to, or from one of its occupants once the room has sent the resource its own presence, until the leave is answered or its grace ends: delivered to that resource (D7's second exception, below). Otherwise the account side (G18) | From a remote room at a verified service the resource's join was sent to, or has just left: delivered to that resource (D7's second exception). Otherwise the account side (G18) |
| MUC domain: service, rooms, occupants, where rooms are hosted for remote occupants | `disco#info`, `disco#items` and ping (`get`), a XEP-0313 query, a `muc#admin` request and a `muc#owner` request (`get` or `set`), and a XEP-0425 `moderate` request (`set`) reach the room host; anything else (registration, vCards, pins, call signalling) `service-unavailable` | Dropped: no room sends a request to a remote entity | Reaches the room host, which accepts a `groupchat` message to a room's bare JID from an occupant and answers any other with an error; an error message from a seated occupant's address ends its seat when it names a delivery failure and is otherwise ignored | Reaches the room host: a join, a change of availability or nickname, a leave; an error presence from a seated occupant's address ends its seat when it names a delivery failure; anything else is ignored |
| MUC domain, on a router without the room host | `service-unavailable` | Dropped | `service-unavailable` | Ignored |
| Spaces PubSub domain, where Spaces are served to remote principals | `disco#info`, `disco#items` and ping (`get`), and a XEP-0060 request in the `pubsub` or the `pubsub#owner` namespace (`get` or `set`) reach the Spaces host; anything else (ad-hoc commands, registration, vCards) `service-unavailable` | Dropped: the service sends no request to a remote entity | `service-unavailable`; an error message is dropped | Ignored |
| Spaces PubSub domain, on a router without the Spaces host | `service-unavailable` | Dropped | `service-unavailable` | Ignored |
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
them (gate G21). `disco#items` lists a service only where remote senders
reach it: the MUC domain on a router that hosts rooms for remote occupants,
the Spaces domain on a router that serves its Spaces to remote principals,
and nothing otherwise. A `node` on either query is `item-not-found`.

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
- for a stanza that reaches the room host, what the host decides for it
  ([Inbound MUC](#inbound-muc-w6a)): an answer queued for the sender's
  domain; a row of the seat table inserted, renamed or removed; a remote
  occupant seated in, or removed from, a local room, with the room's
  presence to its other occupants; for an accepted `groupchat` message,
  the room's own acceptance: its archive row, the inbox projections of its
  local members and one copy for each occupant; and, for an administration
  request of a principal whose standing the effect boundary honours, what
  that request does for a session: an affiliation or role changed, an
  occupant removed, a message retracted by a moderator, the room's
  configuration changed or the room destroyed;
- for a request that reaches the Spaces host, what the host decides for it
  ([Spaces](#spaces-w7a)): one answer queued for the sender's domain; for a
  subscription, a row of the PubSub service's subscriptions and a row of
  `s2s_space_subscriptions`, removed again by its `<unsubscribe/>`; and,
  for a change by a principal whose privilege the effect boundary honours,
  what that request does for a session: a node's configuration, items,
  affiliations or subscriptions changed, or the node deleted;
- for a stanza addressed to a room another replica holds, one message on
  the cluster relay that hands it to that replica with the facts of its
  sender's stream ([Several replicas](#inbound-muc-w6a)); everything above
  then happens there;
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

The `disco#items` answers of the MUC service and of a Space's node list
rooms ([MUC fallback](#muc-fallback-w7b)). They are answers of the
operations above, not further ones, and change nothing: they read the
channel catalog, the live rooms' configuration, the host grants of the
rooms an enrolled scope manages and the Space's items.

Enabling a further service, or a further operation of one, for remote
senders is its own reviewed change to `reach` (or, for room traffic, the
router's `rooms` module), this inventory and the reachability, outbound or
inbound MUC suites, or, for the Spaces service, to `reach`'s Spaces
operations and the Spaces suites. Of D10's enabled set the MUC service and
the Spaces PubSub service are open as described.

## Account traffic in the interim (D7)

This section describes a router without an account side attached. Startup
attaches one whenever S2S is enabled, so in a deployment it describes no
running router; the routing, outbound MUC and self-ping suites run without
one and keep these rules as their evidence. With the account side
attached, account traffic follows
[the account-traffic design](s2s-account-traffic-2047.md) instead, and
the router refuses everything a retired account sends to remote JIDs, IQ
requests and room traffic included, as that design records.

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
The account-traffic suite `s2s_account_router.rs` covers this refusal for
a router without an account side, and what replaces it once one is
attached. The room exception is covered by the outbound MUC suites.

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
| Directional pairs with a queue (`[limits] max_pair_queues` of the S2S snapshot, up to 256) | 64 |
| Pairs a reply to a remote sender, or a remote occupant's join, may open a queue for | half of them: 32 |
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
entity's request to a local entity, completed by the local response. The
account side (W6c) registers a remote principal's request to a local
resource it admits, before delivering it, with a 60-second lifetime;
only that resource's response to the requester with the same id
completes it and leaves, and every other local IQ response offered to the
route is dropped. A replica counts the inbound requests it registered
against its own bounds (1,024, and 256 from one remote domain) until the
response leaves through it or the request expires; expired rows are
pruned with the replica's own. The requests remote entities address to
the MUC service ([Inbound MUC](#inbound-muc-w6a)) are answered by the
service itself on the replica that received them, with the request's own
id, and need no correlation.

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
contract freezes a message's targets and child identities before it is
acknowledged, and lets an unknown started send retry after its deadline.
For a client's own message it is unchanged: a message for an unserved
domain is classified before the ingress authority sees it, so no frozen
target of a direct message is remote. Outbound MUC keeps it so: a groupchat
or private message to a remote room goes to the router from the same
classification and never enters the ingress authority, so it is not
archived, carbon-copied or projected into the inbox by the sender's server
(the room archives groupchat; XEP-0313 §5), and `Accepted` stays transport
custody only.

Inbound MUC adds the one frozen target that is remote: the copy of a local
room's `groupchat` message for an occupant seated from another server. The
room's acceptance freezes it as a peer route like any other occupant's
copy. Executing it hands the stanza to the router's queue for the seat's
domain pair, and the child is recorded delivered at that hand-off: the
record means transport custody, as above, and never that the occupant
received it. Two rules keep this within the retry identity:

- a copy offered again, by a retry of the same accepted message after an
  unknown outcome, is not queued twice: each seat remembers the archive
  identifiers (XEP-0359, stamped by the room) of the last 64 room messages
  queued for it, and a copy that carries one of them is taken and dropped;
- a copy that cannot be queued, or that the stream does not take, is never
  sent later: the seat ends instead, the room removes the occupant with
  status 333, and the occupant reads what it missed from the room's
  archive when it joins again. A copy for an occupant whose seat has ended
  is answered as unavailable and is not retried towards the remote domain.

The window of 64 is per seat and in memory: a retry that arrives after 64
later room messages were queued for the same seat, or after the replica
restarted (which ends its seats), is not recognised. The first is a
duplicate to a remote occupant, which XEP-0359 lets its client discard; the
second finds no seat. When account traffic (W6c) lets an ingress-accepted
message go to a remote account, that slice must record S2S egress as
transport custody, never delivery, and may retry only a stanza the router
reports as never taken; a retry after an unknown outcome would contradict
the retry identity above.

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
resource only when three things hold:

- the resource sent it a join: presence without a type, from a bound
  resource of a local account, to an occupant JID (a bare JID with a
  localpart, and a nickname as resource), carrying the
  `<x xmlns='http://jabber.org/protocol/muc'/>` element (XEP-0045 §7.2.1);
- the JID's domain answered this server's XEP-0030 `disco#info` as a
  multi-user chat service: with the `conference`/`text` identity and the
  `http://jabber.org/protocol/muc` feature, and with no identity of
  category `server` (XEP-0045 §6.2). An account's domain answers as a
  server (`server`/`im`), and a server that also lists the MUC feature on
  its own domain is still a server, so neither is a service;
- the room's own bare JID then answered its `disco#info` with an identity
  of category `conference` (§6.4), or with `item-not-found`: a room that
  does not exist yet, which a join to a verified service creates (§10.1).
  A bare JID at a service that names any other identity (an account, a
  client, a gateway's user) is not a room.

The service is asked first and the room only once the service is known to
be one, so this server never asks an account's bare JID at a server what
it is. Both requests go from the local domain (the server, not the client)
to the domain JID or the room's bare JID, never to an occupant JID, so no
client can answer or take them over. Each is registered in the
correlation table like a client's request, with the server domain as
local address and a generated id, so it completes at most once, on
whichever replica its response arrives, and expires after 30 seconds. Its
response reaches the router as an IQ result or error to the server domain
(the reachability inventory). The replica that completes it caches the
verdict for every replica in `s2s_remote_room_verdicts`, per service
domain and per room bare JID: "room" (a service, or a room) for an hour,
"not a room" for five minutes. For a service, a result without the
identity and the feature or with a `server` identity, and an error other
than a temporary one, are "not a room". For a room, a result without a
`conference` identity, and an error other than `item-not-found` or a
temporary one, are "not a room"; whether a room that refuses its
`disco#info` to non-occupants with another condition is still a room is
not decided by XEP-0045, and such a room is refused. An error of type
`wait` or with a `remote-server-*` condition, an answer the identity
profile refuses, and no answer at all are not cached.

The occupancy is recorded, within its bounds, before anything is asked.
While the service or the room is not known, the join is held in memory on
the replica that routed it, and nothing reaches the JID. When the
service's verdict arrives the join moves on to the room: a cached verdict
on the room decides at once; otherwise the join is held again while the
room is asked, and its deadline starts again. When a verdict arrives (on
this replica at once; from another replica's completion at the next
sweep, at most five seconds later; a replica that sees the request
completed elsewhere before the verdict is cached keeps waiting for it
until the request expires), the join is sent; for a JID whose service or
room is not one the client receives an error presence `item-not-found`
from the occupant JID, with the join's id and the `muc` child; for a JID
that did not answer, `remote-server-timeout`; for a domain that cannot
be reached, the route's own error. A join to a JID whose service or room
is cached as not one is refused at once with `item-not-found`, and a join
whose service and room are both cached as one is sent at once. A join
that may not be held (more than 256 services and rooms being asked on the
replica) is refused with `resource-constraint`, also when it is moved on
to its room. Because nothing reaches an unverified JID, a join's
`<status/>` text never does either.

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
  it is a multi-user chat service, and then the room whether it is a
  room; nothing is sent to the room and nothing from it is admitted;
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
server, not a multi-user chat service, and an account at a service domain
answers as an account, not a room, so the join is never sent. The room's own presence
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
64 stanzas waiting, a worker 256, and one remote domain 64 of a worker's
256, a quarter, so that one peer cannot fill a worker's queue for the rooms
of other peers. A stanza beyond any of these is not dropped: the stream that
carried it stops reading from its peer until the stanza fits (counted as
`waddle.s2s.route.room_traffic.waits`), so its later stanzas wait in the
transport and the peer's writes slow down. A room's burst, such as the
presence of every occupant a join receives before the joining occupant's
own presence with status 110 (§7.2.3), arrives in full and in order,
however many occupants the room has. The 30-second bound applies to each
stanza's wait, not to the stream: a stream whose next stanza also finds the
queues full waits again, so a stream carrying a long burst may wait longer
than 30 seconds in all. A stream whose stanza has not fit within 30
seconds, or that this server shuts down, evicts or replaces meanwhile,
loses that stanza and ends as *lost* (see below): with
`resource-constraint` after the wait (counted as
`waddle.s2s.route.room_traffic.overloaded`), and with `system-shutdown`
when the router stops during the wait. The occupants of rooms behind it are
then told and join again rather than miss a stanza unaware. A stanza that
fits after waiting counts as the stream's traffic once it is taken, so a
stream does not end idle for the time it waited; during the wait itself
the stream can still be evicted as one without recent traffic. A delivery that takes
more than five seconds is abandoned and counted
(`waddle.s2s.route.room_traffic.delivery_timeouts`). A stanza whose
occupancy cannot be read because the shared store failed is dropped
unanswered, since whether it is room traffic is unknown, and counted
(`waddle.s2s.route.room_traffic.store_failures`).

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

**Payloads.** Before a message from the room is delivered, and so before
the recipient pass archives a private message, the router removes what a
client would read as this server's statement about its own account, as
the account side does for a remote account's message
(`crate::s2s::remote_content`): every `urn:waddle:*` payload, the
`urn:xmpp:inbox:1` elements, XEP-0280 carbon wrappers, XEP-0511 link cards
and link-preview requests, and every XEP-0359 `<stanza-id/>` whose `by`
names a domain this deployment serves or cannot be read; the room's own
`<stanza-id/>`, in its bare JID's name, is kept. A XEP-0313 `<result/>` is
kept only on a message from the room's bare JID that is no groupchat, as
the archive a query from the resource names answers (below; XEP-0313
§4.2). XEP-0313 §8.2 has a client check a result's `from` against the
entity it queried as well as its `queryid`, but Waddle's client checks the
`queryid` alone (`MamResultCollector::collect` in `waddle-xmpp-client`), so
the router keeps a result only where its address is the archive's. What a
kept payload nests is cleaned of the same elements, and a message nested
in one (the one an archive result forwards) loses every `urn:waddle:*`
payload directly inside it, as the room's own message does; a deeper
`urn:waddle:*` payload is left to what carries it. Presence from the room
is delivered as it comes: a client reads none of these payloads from
presence.

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
to the occupant (§3.2); the reflection is an IQ request to a local
resource, which the account side admits from a room the resource occupies
or is joining and delivers to the client, whose answer the room relays.
A router without an account side refuses it with `service-unavailable`,
and the room relays that answer, which XEP-0410 reads as "joined, the
client does not implement ping". If the room cannot be reached, the client receives
`remote-server-not-found` or `remote-server-timeout`, which XEP-0410
treats as a timeout.

**XEP-0313 queries.** A query to a room is an IQ request: routed,
correlated and answered with its `<fin/>`. Its result messages come from
the room's bare JID and are admitted as room traffic only to a resource
the router's occupancy record (W6b) holds as joining or joined in that
room: the record is what lets a result through, so a resource without an
occupancy of the room never receives one. That covers a query sent before
joining or after leaving, and another resource of the same account that
did not join. Without an occupancy a result message is account traffic
(W6c): it is cleaned as a remote account's message is, losing its
`<result/>`, and is then handled as a message from a remote sender
without a resource, so a resource still bound receives it with nothing
that answers a query. The client receives the query's `<fin/>` and none
of its results. The `<fin/>` is an IQ response and is delivered outside
the room's queue, so it can reach the client before the last results.

**When a join is not answered.** A join the room has not answered within
60 seconds of being sent ends: any replica's sweep removes the occupancy
and the client receives an error presence `remote-server-timeout` from the
occupant JID with the join's id. A join held for verification on a
replica that stopped ends the same way after twice the verification
timeout from when it was held, or moved on to its room, or replaced by a
later join. A leave's grace ends silently.

**When the stream fails.** The connection manager tells its stanza sink
when an established stream ends, and how:

- *Lost*: a transport or TLS error (`io`, `tls`); end of file, the idle
  timeout or the maximum age while the peer was in the middle of a
  first-level element; a write that did not complete within the write
  deadline; a first-level element the peer left unfinished past the
  incomplete-element deadline; a stanza the router could not queue within
  its deadline or before it stopped (see Inbound); the idle timeout, the
  maximum age, eviction or replacement after the stream waited for the
  router and before it read from the peer again, since what the peer wrote
  meanwhile is still unread and is dropped with the connection; a stream
  error this server sent
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

End of file between elements is *closed* whether or not the peer sent a
TLS `close_notify` first. So a peer that crashed, or whose connection was
cut, between two stanzas looks the same as a peer dropping an idle
connection: its occupants are not told that the stream ended, and learn
that the room no longer seats them only through a XEP-0410 self-ping or
the room's answer to their next stanza. One end that may lose a stanza the peer did write is
reported as *lost* even when the peer wrote nothing: a stream that waited
for the router and then reaches its idle timeout without reading again
cannot tell a silent peer from unread stanzas, and is reported as lost
then.

When an inbound stream from a remote domain to a local domain is lost,
every occupancy of a resource of that local domain in a room at that
remote domain ends, whichever replica routed it: the stream carried the
rooms' traffic for all of them. When an outbound stream is lost, the
occupancies this replica routes end: the stream carried their stanzas.
Each end is queued on the room's worker behind the traffic already
queued, exempt from the queue's bounds and not counted towards them, once
however many failed streams carried the occupancy, and then removes the
occupancy,
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
| Remote services and rooms one replica is asking at once | 256 |
| How long a join waits for each verification answer | 30 s |
| How long a sent join waits for the room's own presence | 60 s |
| How long a room left keeps admitting the room's answer and earlier traffic | 30 s |
| How long a "service" or "room" verdict is cached | 1 h |
| How long a "not a service" or "not a room" verdict is cached | 5 min |
| Room traffic waiting for decision on one replica (four workers of 256) | 1,024 |
| Room traffic waiting from one room | 64 |
| Room traffic waiting from one remote domain, of one worker's 256 | 64 |
| How long a stream waits for one stanza of room traffic to fit | 30 s |
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
service domain and per room asked within the TTLs and is pruned with the
correlation table.

## Inbound MUC (W6a)

Accounts at admitted remote domains use the rooms of this deployment that
are open to other servers (XEP-0045, XEP-0313, XEP-0030, XEP-0199 and
XEP-0410, with XEP-0085, XEP-0115, XEP-0308, XEP-0333, XEP-0359, XEP-0424,
XEP-0425, XEP-0444 and XEP-0461 for what occupants send, pinned at
4132c531487fe9450ae90b905b8df950d0d478b2; ADR-0020 D10). The router's part
is in `crates/waddle-server/src/s2s/route/`: `reach.rs` (which operations
reach the host), `local_rooms.rs` (what the router hands the host and
expects back), `placement.rs` (where a request waits while the replica
that holds its room is found), `seats.rs` (the seat table and the
replica's index of the seats it holds) and `router/hosting.rs` (rates,
queues, seats and their ends). The
host is `server/routes/websocket/s2s_rooms.rs`; it calls the room handlers
client sessions use, each given the evidence it decides with
(`handlers/iq/muc_remote_admin.rs` for administration). With clustering,
`s2s_room_relay.rs` carries a request to the replica that holds its room.

The router decides transport questions only: which stream carried a
stanza, whether its sender holds a seat, the bounds on seats and requests,
and what leaves for a remote JID. Everything about a room is the host's
decision, taken for the remote principal at each request.

**The principal and its evidence.** A stanza reaches the host with the
`RemotePrincipal` the router built from the inbound stream that carried it
([Ingress](#ingress-g10)): the sender's address, which is at exactly the
stream's authenticated domain, and what the stream proved. No session, no
account and no connection of this deployment stands for it. Every decision
the host takes names that stream's evidence (`PrincipalEvidence::of`), and
none takes the stream-less path a local session's request takes:

- The host first decides peer admission for the sender's domain under the
  S2S snapshot the replica runs (`PrincipalPolicy::witness`, ADR-0020 D1).
  A domain that snapshot does not admit, or a stream below what host
  policy requires, is refused with `forbidden` whatever the room holds for
  the account, and a join refused this way leaves no seat. The decision is
  taken here, where the effect happens, not taken from the stream's own
  admission: a request queued before the domain was dropped is refused
  when it is decided.
- A room an enrolled host-grant scope manages decides from its canonical
  grants as honoured for that evidence, through the evidence-naming forms
  of the gate (`resolve_managed_channel_admission`, `HostGrantGate::allows_as`,
  `host_grant_read_still_admitted_as`), each behind the revocation fence's
  ticket. The rules are those of
  [the effect boundary](host-grant-authority-2047.md#remote-principals-at-the-effect-boundary-g17).
- Any other room keeps its own checks (below), with the D1 decision above
  and the trusted-home rule (D8) applied to whatever standing those checks
  give the principal.

The join handler reads the joiner's address from a session value. For a
remote principal the host passes one that names the remote account and
nothing else: it is no credential, is stored nowhere and authorizes
nothing, and the join's decisions name the stream's evidence.

**Which rooms are reached.** A room is reached by remote principals only
while its `federated` setting is on (`RoomConfig::federated`). The setting
is off for every room that existed before it did, and for a new room
unless the S2S snapshot's host policy says otherwise
(`[host_policy] federate_new_rooms`, off by default; it applies when a
room is created by its first occupant or by the channel-creation command,
and never to a room that exists). An owner sets it in the room's
configuration form (`urn:waddle:roomconfig:federated`, a boolean field of
the `muc#owner` form). A room an enrolled scope manages needs the setting
like any other; its grants decide who enters once it is on. A group
conversation of this deployment's accounts is never federated.

The host decides this before anything else about a room, the sender's
grants and standing included. A room that is not open to other servers
answers `disco#info`, a join, a XEP-0313 query and every administration
request with `item-not-found`, in the condition, type and text of the
answer for a room that does not exist, so the answer does not tell the two
apart. The
requests that are answered alike for every address stay so: a ping of a
room's bare JID is an empty result, its `disco#items` an empty list, a message
from a sender without a seat `not-acceptable`, and presence from such a
sender that asks for no seat is ignored.

The setting is read from the room's actor where the replica holds one, and
otherwise from the channel catalog (`channels.federated`, migration 2008),
which the room's configuration and the channel commands keep in step. A
room with neither is not found: no room is created for a remote principal.
While the setting cannot be read the answer is `internal-server-error`
(type `wait`), and a seat the sender holds is left as it is.

Turning the setting off removes the room's remote occupants once the change
is committed: each seat ends, the occupant is sent its own unavailable
presence with statuses 110 and 333, and those who stay see it leave with
status 333 (XEP-0045 §11.1). A request of an occupant the room still seats
then is refused as above and removes it.

**What the host answers.**

| Operation | Addressed to | Answer |
| --- | --- | --- |
| `disco#info` | The service or a room | The description local sessions get, reduced to its identities and to the features `disco#info`, `disco#items`, `http://jabber.org/protocol/muc` and a room's XEP-0045 type flags (`muc_*`); no extension form. A room an enrolled scope manages is described to an asker its grants admit and is `item-not-found` to any other. A room that does not exist or is not federated, and any `node`, is `item-not-found`. A description that could not be built is `internal-server-error` (type `wait`), which a peer does not cache as a statement about the address |
| `disco#items` | The service | The rooms the asker may reach and be told of, by their plain JIDs, at most 256 in one answer and paged with XEP-0059, with no count when the listing left a room out ([MUC fallback](#muc-fallback-w7b)). `internal-server-error` (type `wait`) when the listing cannot be built. A query that is not well formed is `bad-request` |
| `disco#items` | A room | An empty list, the same for every room address: a room's occupants are not listed to remote askers (XEP-0045 §6.5 allows it). A `node` or an occupant JID is `item-not-found` |
| Ping | The service or a room's bare JID | An empty result |
| Ping | The sender's own occupant JID | The XEP-0410 self-ping, answered from the sender's seat, the room's grants and its occupancy: a result while joined, `not-acceptable` otherwise, `remote-server-timeout` (the error XEP-0410 §4 has a client take no decision from) while a managed room's grants cannot be read, the room's actor cannot be reached or it does not answer |
| Ping | Another occupant's JID | `service-unavailable`: relaying it would reach an account (D7) |
| XEP-0313 query | A room's bare JID | Admitted as entry to the room is, for the asker and the evidence of the stream that carried the query, before the archive is read and for a managed room again after it; the results, from the room's bare JID to the asker, then the `<fin/>`. `forbidden` when not admitted, `item-not-found` for a room that does not exist or is not federated, `service-unavailable` for an occupant JID |
| Presence without a type | An occupant JID | A join when the sender holds no seat in the room, otherwise a change of availability or nickname |
| Unavailable presence | An occupant JID | A leave, for a sender that holds a seat |
| `groupchat` message | A room's bare JID | From a sender that holds a seat, what it carries of the standard extensions is accepted (below); `not-acceptable` from any other sender (XEP-0045 §7.4). A subject change is put to the room only where the room lets every occupant change it or the sender's moderator standing is honoured for the stream, and is `forbidden` otherwise (§8.1), or `internal-server-error` (type `wait`) while the room's configuration cannot be read. `resource-constraint` (type `wait`) once the account has spent its allowance of archived messages. A message that carries nothing a room relays is neither relayed nor answered |
| Any other message | A room or an occupant | `feature-not-implemented`: private messages, invitations, voice requests and declines. A headline, and a message of any type that carries only a chat state or a marker, is neither taken nor answered |
| `muc#admin` request | A room's bare JID | The affiliation and role lists and changes to them (kick, ban, role, affiliation), decided for the requester on its stream's evidence (below). `forbidden` without a standing the effect boundary honours, `not-allowed` for a change D8 does not issue, `item-not-found` for a room that is not federated |
| `muc#owner` request | A room's bare JID | The configuration form, a configuration change and the room's destruction, for an owner whose standing is honoured; `forbidden` otherwise. A managed room is configured by its grants' `manage` and never destroyed by a request |
| XEP-0425 `moderate` request | A room's bare JID | A moderator's retraction of another occupant's message, for a requester whose room moderation is honoured; `forbidden` otherwise, `bad-request` for an element that asks to retract nothing |
| Error presence or message | A room or an occupant | From a seated occupant's address with a delivery-related condition of XEP-0045 §17.1.2 (`gone`, `item-not-found`, `recipient-unavailable`, `redirect`, `remote-server-not-found`, `remote-server-timeout`): the seat ends. Otherwise ignored, `service-unavailable` included (below) |

Every error answers with the id of the request it refuses (RFC 6120
§8.1.3), the error presence that refuses a join included; the room's own
presence for an occupant carries none, as for a session.

**What an occupant's stanzas carry.** Presence and messages from a remote
occupant are rebuilt from an allowlist before the room sees them
(`reduced_presence`, `reduced_message`); what the allowlist does not name
never reaches the room's pipeline, whose handlers act on payloads.

Presence carries its availability (`show`, `status`, `priority`), its
entity capabilities (the XEP-0115 `<c/>` element, rebuilt from its
attributes) and, for a join, the bare `muc` element (XEP-0045 §17.1.1 lets
a service reduce presence). This deployment's presence extensions (calls,
hats) mean something only for its own sessions, and a history request is
not honoured. A `muc#user` element is not carried inbound: XEP-0045 has
the room author it (§7.2.2), the room strips one from a session's presence
before it adds its own, and the router reads the status codes of the
room's presence to follow a seat. The room's own presence for and about
the occupant carries the room's `muc#user` element as for any occupant.

A message carries its bodies, its subject, its thread and these payloads,
each kind once:

| Extension | Carried | The room's check, as for a session |
| --- | --- | --- |
| XEP-0359 | `origin-id`. A `stanza-id` is the room's to assign and is not carried | The room stamps its own `stanza-id` |
| XEP-0308 | `replace` | Only the original sender, under the same occupancy (§3) |
| XEP-0424 | `retract` | Only the original sender (§4). A moderator's retraction is XEP-0425, an IQ (above) |
| XEP-0444 | `reactions` | The sender is an occupant |
| XEP-0461 | `reply`, with the XEP-0428 `fallback` that marks the quoted part of the body (it may repeat) | The identifier and the address the reply names are well formed |
| XEP-0333 | `markable`, `received`, `displayed`, `acknowledged` | The sender is an occupant |
| XEP-0085 | `active`, `composing`, `paused`, `inactive`, `gone` | The sender is an occupant |

"The original sender" needs more than the room's check for a session gives
it. The room compares the sender's occupant address with the archived
one, and an occupant at another server chooses its nickname, so it could
enter under a nickname that somebody else's archived messages carry. A
correction or retraction is therefore the sender's own only when the
archived message records the sender's account as its author (the real JID
the room records for a non-anonymous archive, XEP-0313 §MUC Archives).
Where a message records its author this holds for every sender; a message
that records none keeps the address comparison for an address this
deployment serves and is not revised by a remote occupant.

Dropped from a remote occupant's message: this deployment's own payloads
(pins, forum actions, framework envelopes and everything else in a
`urn:waddle:` namespace), mentions (XEP-0513) and references (XEP-0372),
which notify accounts, processing hints (XEP-0334), which would decide
what is archived, and every payload the allowlist does not name.

The XEP-0313 answer is given although the feature is not advertised to
remote askers; gate G21 decides what is advertised.

**Seats.** A seat is one remote full JID's occupancy of one local room on
the replica that holds the room's actor. It records the room, the
occupant, its nickname, the occupancy generation the room seats it under,
and what the stream that carried the join proved: the authentication
strength, the trust profile, and the generation of the snapshot the stream
was admitted under. The replica keeps the seats it holds in memory
(`SeatIndex`), which is what egress and every decision that names a seated
remote occupant read; the shared table `s2s_local_room_seats` (migration
2007) carries the bounds across replicas and lets another replica find and
end a seat. Those decisions reach the index through the app state's
host-grant gate, bound to the ingress authority's attachment gate when the
replica starts hosting rooms, on a deployment without a canonical grant
store (SQLite) as on one with it: unbound, the gate would report no seat,
refuse the moderator role to every remote occupant and let an owner or
admin standing be issued for an account seated on a stream that does not
honour it.

- *Opening.* A join from a sender without a seat opens one, within the
  bounds (below), before the host decides it: the row is inserted and the
  seat held as pending. A seat is opened only where the room's stanzas for
  it can leave: a place on the queue of the pair from the room's domain to
  the occupant's, which a join opens within the bound on queues remote
  senders open, and holds until the seat is held; otherwise the join is
  refused with `resource-constraint` (type `wait`) and the room is not
  asked. The host seats the occupant
  under the seat's occupancy generation and says whether the room seated
  it. A join the room refused, or whose answer could not be read (the
  occupant is then removed again), drops the seat.
- *Entering again.* Available presence from an occupant that holds a seat
  is a change of availability where the room's update path takes it.
  Presence that enters instead (it carries the `muc` element, or the room
  no longer seats the occupant) is decided as a join: the host says so
  without deciding it, and the router opens a seat under a new generation,
  held as pending, which takes over the row and the place of the one the
  occupant held, and asks again. If the room does not seat the occupant
  again, it is told to remove the occupancy of the seat that was replaced.
- *Order.* While a join is being answered, what the room sends the new
  occupant through the delivery path is kept, up to 256 stanzas, and
  queued behind the join's answer, so the occupant receives the room's
  roster and its own presence before anything else (XEP-0045 §7.1). More
  than that ends the seat.
- *Following the room.* The seat follows the room's own presence for the
  occupant as it leaves: unavailable presence with status 110 and without
  status 303 ends the seat (a leave, a kick, a ban, a revoked membership,
  the room's destruction); with status 303 it records the new nickname. In
  the answer to the occupant's own request that presence ends the seat
  once it is queued. When it reaches the router through the delivery path
  instead, nothing on it says which occupancy it is for, so the room is
  asked first, in the room's order: the presence is sent, and the seat
  ended, only if the room no longer seats the seat's generation. Presence
  for an earlier occupancy of an occupant that has entered again (a room
  effect delivered a second time, a departure that was retried) is
  dropped: it neither ends the new seat nor reaches its occupant. When the
  room cannot say, the seat ends as for an undeliverable stanza.
- *The generation.* The occupancy generation is minted with the seat,
  recorded in its row and given to the room, which seats the occupant
  under it; a removal, a check and a take-over all name it. A room's
  stanza does not carry it: the stanzas a room sends an occupant name the
  occupant's full JID, and the room's effect records name sessions the
  same way. What that leaves is a copy of an older stanza that does not
  end an occupancy and is delivered again after the occupant entered
  again: a message is told from one already sent by its archive
  identifier, any other such stanza (a presence update) is sent to the new
  seat.
- *Evidence.* A stanza from a seated occupant that arrives on a stream of
  another strength or trust profile is not the seat's: the seat ends, the
  occupant is told (below) and the stanza is decided as one from a sender
  without a seat, so presence enters again on the evidence of the stream
  that carries it now.

**Egress.** A local room's stanza leaves for a remote JID in two ways
only:

- as the host's answer to that JID's own request, from an address of the
  MUC service, to the sender of the request;
- as the room's traffic to a seat this replica holds: presence, and
  `groupchat` messages. A full JID at a domain this deployment serves, or
  in a room without a seat held here, is told apart without the seat
  index's lock, so a local sender's own copy of its message costs no
  lock. The connection registry offers a stanza for a full
  JID that holds no connection to the router (`ForeignDelivery`), which
  takes it only when the JID holds a seat in the room the stanza comes
  from. The room's effect outbox, which delivers what a durable mutation
  caused (a kick, a ban, an affiliation or configuration change, the
  room's destruction), offers a recipient without a connection the same
  way; a copy the router takes needs no write acknowledgement. Private messages, invitations and IQs between occupants are not
  taken, since their answers would be account traffic (D7); they find no
  recipient, as before.

One stanza is queued for one seat, without waiting, on a queue of the
seat's directional domain pair that holds only local rooms' stanzas
(4,096 by default). Nothing fans out after that point and no recipient is
resolved again.

A remote occupant can make a room send to the seats of every other remote
domain: its messages, and its presence, since a join, a change of
availability or nickname and entering again each make the room send the
occupant's presence to every occupant. What it can have queued for them
is bounded where nothing depends on its delivery. A room message that
carries only a chat state
(XEP-0085) or a marker that answers a message (XEP-0333), with no text and
no subject, is dropped for a seat when the queue for the seat's domain is
more than three quarters full or has no place, and beyond an allowance of
such copies for its room (1,024 a second, four seconds' worth at once) and
for its sender (256 a second, on the same terms). Dropping one ends no
seat. Every other stanza that finds no place ends the seat, as below. The
other stanzas a remote account can make a room send are bounded where
they enter: the messages it can have archived (Messages), and its
presence by the account's share of presence (one a second on average,
64 at once, below), which a join, a change of availability or nickname
and entering again all count against. The pair's worker checks again before it writes: peer
admission must admit the remote domain now, and a room's stanza must be
for a seat still held under the same occupancy generation, or be the
presence that ends it. So nothing a room emits reaches a remote JID it
does not seat, and a remote entity cannot have a room send to a third
party: a room's traffic goes to its occupants, and an answer goes to the
sender of the request it answers.

**How a seat ends.**

| Cause | What happens |
| --- | --- |
| The room's own presence ends the occupancy (leave, kick, ban, revocation sweep, room destroyed) | The presence is sent to the occupant with its status codes; the seat is released and its row removed. Presence that did not come as the answer to the occupant's own request is sent only once the room confirms that it no longer seats that generation, asked within the per-item budget; a room that cannot say ends the seat as for an undeliverable stanza |
| A stream between the room's domain and the occupant's domain is lost (`StreamEnd::Lost`, as [stream ends](#outbound-muc-w6b) are classified) | Every seat of that pair this replica holds ends: the room removes the occupant and tells those who stay with status 333 (XEP-0045 §11.1). Nothing is sent to the removed occupant; its server saw the stream fail. Seats of the pair that another replica holds are marked in the table, and their owner ends them at its next sweep, within five seconds: it gives each seat up before it queues the seat's end, so a marked row that is read again before that end removed it queues nothing more. A stream that is closed in order ends no seat |
| A stanza of the room cannot be queued for the seat, no stream can be established for it, or the stream does not take it | The seat ends at once, so nothing more is queued for it; the room removes the occupant with status 333; the occupant is sent its own unavailable presence with statuses 110 and 333 if a place is free for it |
| The occupant's server answers the room with a delivery error | The room removes the occupant with status 333 (§17.1.2, "ghost users") |
| A stanza of the occupant arrives on a stream that proved something else | As for a stanza that cannot be queued |
| The room's `federated` setting is turned off | Every seat of the room ends once the change is committed: the room removes the occupant with status 333 and the occupant is sent its own unavailable presence with statuses 110 and 333 |
| The host refuses a request of a seated occupant in a way that ends its occupancy (its domain is no longer admitted, its room is not open to it any more, its leave could not be answered) | The seat ends, the room is told to remove the occupant, with status 333 for those who stay, and the occupant is sent its own unavailable presence with statuses 110 and 333 after the refusal. Where the request was presence that entered the room again and the room refused that join, the refusal itself tells the occupant that it is not in the room, and it is not told again |
| The host does not decide a join within the per-item budget (5 s) | The join is answered with `internal-server-error` (type `wait`); the seat opened for it, and one the occupant gave up to enter again, end without a further notice, and the room removes the occupant if it seated it. Any other request the host does not decide in time is answered as its kind is answered when it is refused before it is taken: an IQ, or a message that carries something, with `internal-server-error` (type `wait`), presence that enters the room from a sender without a seat with error presence, and nothing else; a seated occupant's presence is not answered, so its seat stands. A request whose durable effect the host had begun (a message's commit, an administration, configuration or moderation change) is answered only if it is an IQ, with `internal-server-error` saying that it may have been applied; a message is not answered, since the room may have accepted it and the ingress authority recovers its effects |
| The room no longer seats the occupant (its actor ended, or it removed the occupant without its presence passing the router) | Every 60 s a pass asks each room with seats, once, for the occupants it seats and their generations, and releases the seats the room does not name; the occupant is sent its own unavailable presence with statuses 110 and 333. A pass runs on a task of its own, takes at most 256 rooms and 60 s, and the next goes on where it stopped |
| The room seats an occupant of another server for which this replica holds no seat, pending seats included (its seat ended and the room's removal timed out, or the room was restored with it) | The same pass removes it from the room, with status 333 for those who stay: nothing could reach it. Rooms whose removal of an occupant could not be confirmed are asked at the next pass first, whether or not a seat is still held there. A seat of the same occupancy that ends while the removal waits or runs, or after it ran (a seat taken over from a replica that stopped, held a moment after the room was restored), is not dropped as a second end: it takes the removal's place, or follows it, so its row is removed and its occupant told as for that seat; the last 4,096 occupancies removed this way are remembered for that |
| The seat's row could not be removed when the seat ended | The same pass reads this replica's rows, a page of 256 in the order of the table's key, and removes those older than 30 s for which it holds no seat |
| The replica stops | It tells each occupant it seats, with the occupant's own unavailable presence and statuses 110 and 332 (XEP-0045 §11.2), gives the seats up and waits up to 2 s until the pairs' workers have handed those notices to their streams or given them up, and the streams drain only after that; nothing is promised for a notice no stream takes. A seat whose join is still being answered is given up without a notice. Whether that join is answered as entered is decided against the farewell under the seat index's lock: one decided after the farewell gave its seat up is answered with `resource-constraint` (type `wait`), and its occupant is never told that it entered; one decided before is answered as entered and then, after that answer on the same queue, told with statuses 110 and 332, and the farewell waits for that notice as for the others. No seat is held, adopted or opened once the farewell has given the seats up. Its rows are removed at shutdown, within the shutdown budget. Rows a replica left without shutting down are removed by another replica once the owner has not recorded that it is alive for ten minutes; a join for the same room and occupant that reaches a live replica replaces such a row after 30 s, whatever the seat bounds say, since the row is the join's own |
| The replica that held the room stopped without shutting down, and the room's claim moved | The replica that takes the room over takes over the seats of its recorded remote occupants whose row names the recorded occupancy generation, so that the room can seat them as recorded, and ends each of them once the room has: what the stopped replica had queued for an occupant is lost and cannot be told from what it received. The room removes the occupant with status 333 for those who stay, and once that removal has removed the recorded generation the occupant is sent its own unavailable presence with statuses 110 and 333 (unless it has entered again meanwhile) and enters again. A recorded occupant whose seat stands for another generation is not seated again, and the row taken over for it is removed; so is a recorded occupant for whom a join opened a seat while its row was taken over, whose join's seat is kept. A room whose actor is restored on the replica that already seats its occupants keeps those seats: an occupant is seated again on the seat it holds under the recorded generation, and nothing ends |
| The hosting replica's snapshot no longer admits the domain | The occupant's next request is refused, nothing more is written to it, and for a managed room the revocation sweep removes it ([Seated occupants](host-grant-authority-2047.md#seated-occupants-roles-and-media-g14)). An occupant of an unmanaged room that sends nothing keeps its entry in the room until its stream is lost or the replica restarts, which a changed snapshot requires (below) |

A seat's end gives the seat up at once, and tells the occupant where the
table says so, on whichever worker found it. The room's removal of the
occupant and the removal of the seat's row run on workers of their own
(four), each within the per-item budget, one end for each occupancy
however many causes found it. No worker that hands requests to the room
host waits for them, so a removal may run beside a later request of the
same occupant: every removal names the generation it removes, and a
later join seats the occupant under another.

**Messages.** A `groupchat` message from a seated remote occupant is
planned as the room plans any occupant's and committed through the ingress
authority with a principal of its own kind (`IngressPrincipal::Remote`:
the remote account and the seat's stream evidence), so the room's
acceptance decides the sender on that evidence and freezes the audience as
for a local sender. Nothing a local session's plan reads from its stream
takes part: no carbons, no blocking list, no stream-management position,
and no archive row of the sender's own (its server keeps that). The room's
plan is otherwise the one every occupant's message gets: the room's
archive row, inbox projections for the room's durable members, and one
copy for each occupant.

The sender's own copy (XEP-0045 §7.4) is, for a session, a frame its
connection writes, and it is returned as the answer to the request. A
remote sender has no connection, and an answer is queued only after every
effect of the message has run, behind the copies of later messages the
room sent its seat meanwhile. So the frame is also handed to the sender's
seat when the message's effects reach it, among the copies of the same
message for the other occupants (the effects of one message run one after
another, in the order the room planned them, local occupants' copies
first), and the answer does not repeat a copy handed to the seat while the
request was decided (it is told by the identifier the room's archive gave
the message). A message the occupant sends again is answered again. The
sender's copy is then ordered among other messages' copies as any
occupant's is: the effects of two messages accepted at the same moment
are not serialised against each other, for any occupant.

The enrichment hooks of this deployment's extensions do not run for a
remote occupant's message: they run for the messages of its own accounts,
and none has been reviewed to act for a principal that holds no account
here.

Nothing is kept for an account at another server beside what the room
itself keeps. The inbox projection and the activity projection write only
for an address at a domain this deployment serves: a remote sender gets no
inbox row of its own, a remote durable member none as a recipient, and a
join, a leave, a chat state or a displayed marker of a remote occupant
records no activity and marks no row read. Its inbox, its devices and its
read state are its own server's.

What a remote account can have archived is bounded for the account as well
as for its domain: five messages a second on average and thirty at once,
counted on the replica that writes the archive, for a message of a kind
the room archives (text, a subject, a correction, a retraction, a
reaction) when it is put to the room, whether or not the room then accepts
it. A chat state or a marker is not counted. A message beyond that is
refused with `resource-constraint`
(type `wait`) and counted (`waddle.s2s.rooms.archive_writes.limited`).
The allowances are kept for 65,536 accounts; an account that finds the
table full of accounts still spending theirs is refused the same way.

**Administration.** A remote principal administers a room with a standing
the effect boundary honours for the stream that carried its request
(ADR-0020 D8): an owner or admin affiliation, or the moderator role, of an
account at a trusted home on a `pkix` stream, under a private CA where the
home's entry requires one. Its `muc#admin`, `muc#owner` and XEP-0425
requests run through the handlers a session's requests run through, with
that stream's evidence in place of none and no session at all:

- A room an enrolled scope manages decides the requester from its
  canonical grants through the evidence-naming forms of the gate
  (`allows_as`, `standing_as`, `still_allows`). An affiliation change is
  recorded through the store on the requester's evidence
  (`set_affiliation_as`), which decides the initiator again under the
  scope's guard, and is answered once the revocation fence has
  acknowledged it. A role change is applied by the room actor only while
  the decision's fence ticket is current. A configuration change reads the
  grants again under the room's configuration lock and once more just
  before the actor applies it. A managed room is never destroyed by a
  request.
- Any other room decides from the standing it holds for the requester, as
  D8 honours it (`honoured_legacy_affiliation`, and the moderator role
  only with honoured privilege): the room actor applies the request under
  that standing. An owner whose standing is honoured configures and
  destroys the room.
- What D8 does not issue is refused whoever asks: an owner or admin
  affiliation for an account of a home that is neither served nor trusted,
  and the moderator role for an occupant whose seats do not honour room
  moderation (`not-allowed`).
- Everyone else is refused with `forbidden`, as XEP-0045 says for a
  requester without the standing (§8.2, §9, §10).

A remote principal's configuration request writes no authorization
relationship for it; a session's writes the channel's owner relationship
for its account. The answer and the requester's own copies of what the
request caused (its own presence after a role change, the room's
announcement of a retraction) are the reply to the request, in the order a
session receives them; what the request caused for other occupants leaves
by the room's delivery path. A session's connection settles the room's
effect records for its own copies once its writer has taken them; for a
remote requester they are settled when the answer is handed to the
router's queue for its domain, which is all the custody a reply to a
remote sender has.

**Several replicas.** A room's actor lives on the replica that owns the
room's claim, and a peer's inbound stream lands on whichever replica
accepted it. A stanza for a room another replica holds is carried there
before anything is decided or seated for it where it arrived
(`LocalRooms::place`), so a seat is only ever on the replica that holds the
room.

Placing a request asks another replica, which may be slow or gone, so it
is done in a stage of its own (`s2s/route/placement.rs`) between the
stream and the workers that hand requests to the room host. Those workers
serve the rooms of every remote domain in turn and never wait for another
replica. Requests for one room wait in one lane, in arrival order, and are
placed one at a time, so they reach the room's queue, or its holder, in
the order they arrived. Lanes are placed concurrently: 64 rooms at once on
a replica, and 8 for the requests of one remote domain, so a domain whose
rooms are held by a replica that does not answer leaves the places of
every other domain alone. Waiting for one of those places and asking the
holder are bounded apart, each at 5 s. A request that finds no place in
time is answered with `resource-constraint` (type `wait`) alone: the wait
says nothing of its room's holder. The ask does. The relay's own bound on
placing one request is 4.5 s in all (1 s for the holder to take the
request, 3 s for its answer and half a second for the lookups), covering
the lookups of the room's claim and catalog row and both asks when the
claim moved between them, below the router's 5 s. A holder that does not
answer an ask begun with its two windows (4 s) still ahead, or cannot be
reached, is reported as such by the replica that asked
(`RoomPlacement::Silent`); time that runs out during a lookup, before an
ask could begin, or during an ask begun with less than those windows
left, is answered with `resource-constraint` and says nothing of the
holder, which could have answered in its windows and still too late. The
router's 5 s is
a backstop. Either way the request is answered with `resource-constraint`
(type `wait`), and so is everything that waits for the same room then;
for the next 5 s requests for that room are answered the same way
without asking again, so a silent holder costs each of its rooms one wait
and not one for every stanza. A holder that answers without taking the
request (its queues are full, it no longer holds the room) is not taken
for silent: that one request is answered. No stream is closed and no
seat ends for a holder that does not answer. A leave, or an error that
ends a seat, that cannot be placed is not dropped and not answered: it is
kept, one for each room and sender (a leave replaces an error), within
the bounds of what waits to be placed, and placed again at each sweep
once its room's holder is no longer taken for silent, for up to 120 s
from when it was first kept, however often it is placed again. Available
presence from the same sender for the same room that is placed from this
replica meanwhile, such as a join, forgets it, whether it is kept or back
in the room's lane, so a leave from before a later join never ends that
join's occupancy on the room's holder; a later presence that is not
placed in the end forgets nothing. Kept leaves are bounded like what
waits to be placed (below), and for one remote account at a quarter of
its domain's share (16 by default), never above the seats one account
may hold. One that finds no place to be kept, or whose time passes, is
dropped and counted. What
waits to be placed is bounded like the queues behind the stage (1,024 on a
replica, 32 for one room, 64 from one remote domain), and as there a
stanza beyond the replica's or its domain's bound holds its stream until
it fits, while one beyond its room's is answered as its kind is and holds
none, so a room whose holder is silent fills only its own lane. Stanzas
for the service
itself are not placed, and a host that keeps every room itself (a single
replica) is asked nothing. The stanza is
carried by one relay message,
`waddle.clustering.relay.remote_room_request.v1`, with the facts of the
stream that carried it (the sender's address, the authentication
strength, the trust-profile kind and the snapshot the stream was admitted
under) and nothing decided from them.

The holder builds the principal from those facts (`RemotePrincipal::carried`),
checks that the stanza is from that address, for a room at a MUC domain it
serves and one of the operations a remote principal reaches, and queues it
behind the room's earlier requests. From there it is a request like one
from a stream of its own: the host decides peer admission and the sender's
standing again under the snapshot the holder runs, the seat is opened and
held there with the carried evidence, and the answer and all later traffic
for the occupant leave through the holder's router and its own stream to
the occupant's domain, in the room's order. A later stanza carried with
other evidence ends the seat as on a single replica. A carried stanza is
never carried again; a holder that no longer owns the room says so, and
the carrying replica looks the owner up once more.

A room is carried only when it may be open to other servers: its row of
the channel catalog says so, or it has no row while the deployment opens
its new rooms to other servers (`[host_policy] federate_new_rooms`). A
room the catalog says is not federated, a group conversation, and, while
that default is off, an address without a row are not carried: the replica
the stanza reached answers them as a room that does not exist, whatever
the state of the room's holder, so a remote sender cannot tell a closed
room from a missing one by whether a holder answers and cannot make a
replica ask another for any address it names. The same holds where the
room's actor cannot be reached on the replica that decides (it is being
restored, or its claim is moving): the answer is `internal-server-error`
(type `wait`) only for a room that may be open, and `item-not-found` for
any other address.

What this leaves: a room without a catalog row that an owner opened by
hand in its configuration form, in a deployment whose default for new
rooms is off, is reached only through the replica that holds it; a
request that lands on another replica is answered `item-not-found`. Such a
room has no row to record the setting in. With the default on, an address
without a row is carried, and a holder that does not answer then tells a
remote sender that some replica holds a room of that name.

The relay's reply says only whether the holder queued the stanza. When it
did not (its queues are full, it no longer holds the room, it hosts no
rooms, it could not be reached, it did not answer in time), the carrying
replica answers the sender with `resource-constraint` (type `wait`); the
last two are the silent holder above, whose room is then answered without
asking for the backoff. The
carrying replica spends its domain's request rate, and the sender's
account's share of it, on the stanza; the holder does not spend them
again. The holder takes a carried stanza only for a room at its MUC
service's domain.
Stanzas for the service itself are answered by the replica they reach. A
stream lost on the carrying replica marks the pair's seats in the shared
table, and their holder ends them at its next sweep, as before.

The relay message is a new message id. The ordered relay's envelope and
its id (`waddle.clustering.relay.deliver_ordered.v16`) are unchanged, and
no stored room request carries a remote principal: the mailbox and the
ordered relay name a session's principal without stream evidence and hand
the answer back to the replica that asked, which would send a join's
answer and the room's traffic on two streams. During a rolling update a
holder that predates the message answers `UnknownMessage` before any
handler runs, and the carrying replica answers the sender with the
retryable error; a carrying replica that predates it decides the stanza
itself and refuses a join or a message for a room it does not hold, as it
did before. Neither order seats an occupant twice or decides a request
without its evidence.

**Rooms an enrolled scope manages.** Beyond the decisions above:

- the room's acceptance decides the sender on the evidence its submission
  carries (`RoomAcceptance::sender_evidence`) and each named remote
  recipient on the evidence its seats were admitted under (`SeatEvidence`),
  so a remote occupant is in the audience exactly while every stream its
  seats were admitted on would admit it to read;
- the revocation sweep decides a seated remote occupant the same way, and
  removes it when its grants no longer admit it; the room's presence for
  it, with status 321 or 301, leaves over S2S and ends the seat;
- re-seating after a claim move seats a recorded remote occupant again
  only if the replica takes over its seat, which it can once the seat's
  owner is no longer alive, and decides it on the seat's evidence. This
  holds for every room; in a room no scope manages nothing further is
  decided.

**Rooms no scope manages.** These keep their legacy checks, and the
following is applied to a remote principal on top of them:

- D1 at every request, as above;
- no room is created: a join for a room that has neither a live actor nor a
  channel row is `item-not-found`, since creating a room makes its creator
  the owner;
- a group conversation of this deployment's accounts is never federated,
  so it is not found;
- D8 where the seat is taken: a stored owner or admin entry counts only for
  a trusted home on a `pkix` stream (`honoured_legacy_affiliation`). An
  entry that is not honoured contributes nothing, and since the room actor
  would count it, the join is refused with `forbidden`; a join that the
  room nevertheless seated with an owner or admin affiliation or the
  moderator role, without the witness honouring it, is undone before
  anyone is told;
- D8 at issuance, in the `muc#admin` writer and in the admin command that
  sets a room affiliation (`channels:set-affiliation`): an owner or admin
  affiliation is granted only to an account of a domain this deployment
  serves or of a trusted home, and the moderator role only to an occupant
  whose seat's evidence honours room moderation; otherwise `not-allowed`.
  A room gives every occupant of an owner or admin the moderator role and
  decides that for the account, not for each of its seats, so an owner or
  admin affiliation is also not granted while one of the account's seats
  in the room would not honour it (`seats_honour_privilege`;
  `not-allowed`): it is granted once that occupant has left or has entered
  over a stream that does. The `muc#admin` writer runs on the replica that
  holds the room and reads its seats; the command may run on any replica,
  and reads the account's seats in the room from the shared seat table as
  well (`seats_honour_privilege_anywhere`), refusing when the table cannot
  be read. The same check is applied in a room an enrolled scope manages. Refusing the grant was
  chosen over granting it and holding back the role of single seats: the
  room actor derives one role for an account, and the durable
  administration batch records what the actor applies;
- a join the room seated on a standing that is not honoured is undone
  without anyone being told that the occupancy ended
  (`MucRemovalCause::Unannounced`): the occupants present when it was
  seated had not been sent the joiner's presence, and the joiner is
  answered with the refusal. An occupant that entered in between had
  been, in the answer to its own join: the occupants the room seats once
  the join is undone that it did not answer the joiner with are sent the
  joiner's unavailable presence with status 333. One that entered after
  the undoing is told of an occupant it never saw, which clients ignore;
  one that left and entered again under the same full JID in between is
  not told, nor is one that now holds the joiner's nickname, whose own
  occupant JID that presence would name. The room is read once, within
  2 s, for its occupants and the holders of the nickname together, and
  the notices go out on a task of their own, so the router's bound on
  deciding the join does not cut them short;
- D8 at the request, for administration: the standing the room holds for a
  remote requester counts only as honoured for the stream that carried the
  request (above);
- a remote principal's entry never repairs the permission graph;
- everything else is the room's own: members-only rooms admit a remote
  account only with a stored membership, a ban stands on every stream, and
  a federated room that is not members-only admits any account of an
  admitted domain as a participant and serves it the archive.

A mention is not among the payloads carried from a remote occupant, so
nobody is notified as mentioned, and the channel-wide mention, which a
managed room decides for its sender without stream evidence, is never
reached.

**Bounds.**

| Bound | Default |
| --- | --- |
| Remote occupants one local room seats | 256 |
| Seats of one remote domain | 4,096 |
| Seats of one remote account, under every resource | 64 |
| Seats of the deployment | 65,536 |
| Stanzas one remote domain may address to the MUC service, on average and at once | 50 a second, 200 |
| Of those, the stanzas of one remote account other than joins, on average and at once | 10 a second, 40 |
| Of those, the available presence of one remote account to occupant JIDs (joins, changes of availability or nickname, entering again), on average and at once | 1 a second, 64 |
| Of those, the leaves, errors and ignored presence of one remote account, on average and at once (beyond it they are dropped, except the first leave for each seat held here) | 10 a second, 40 |
| Refusals of an account's stanzas beyond its share, and of requests for a full room, that are answered, for one remote domain | 5 a second, 20 |
| Refusals of one remote account's joins beyond its share of presence that are answered (beyond it they are dropped) | 0.1 a second, 6 |
| Rooms being placed at once on one replica, and for the requests of one remote domain | 64, 8 |
| How long a request may wait for a place among the rooms being placed, how long asking the holder may take, and how long a room whose holder did not answer is answered without asking | 5 s, 5 s (4.5 s for the relay's lookups and asks together), 5 s |
| Requests waiting for the room host on one replica (four workers of 256) | 1,024 |
| Requests waiting for one room, never above half of a domain's share (a stanza beyond it is answered as its kind is, and holds no stream; a leave and a delivery error are not counted) | 32 |
| Requests waiting from one remote domain, of one worker's 256 | 64 |
| How long a stream waits for its domain's rate or a place | 30 s |
| How long one item of a worker that hands requests to the room host may take: deciding a request, its seat's changes and queueing its answer; and each step of a seat's end | 5 s |
| Messages one remote account may have archived, on average and at once | 5 a second, 30 |
| Remote accounts whose archive allowance is kept on one replica | 65,536 |
| How long the holder of a room may take to queue a carried request | 1 s to take it, 3 s to answer |
| Local rooms' stanzas waiting for one directional pair's stream | 4,096 |
| How long an answer waits for a place among them | 10 s, never past the per-item budget |
| Copies of chat states and markers queued for remote seats, for one room and for one sender, a second (four seconds' worth at once) | 1,024, 256 |
| Remote domains one replica seats occupants for (the pairs a remote sender may open a queue for, below) | 32 |
| How long a stopping replica waits for its shutdown notices to be taken | 2 s |
| Room stanzas kept for a seat while its join is answered | 256 |
| How often seats are checked against their rooms, and how many rooms one pass asks | 60 s, 256 |
| How long a seat's owner must have been silent before a join elsewhere replaces it | 30 s |

A join beyond a seat bound is refused with `resource-constraint` (type
`wait`). Whether a seat for the room and occupant exists is read first,
and answered before any bound: the join then takes that row over or
leaves it to the replica that holds it, and the row never counts against
the join that replaces it. The bounds of the room, the remote domain and
the account are counted and the row inserted in one transaction; on
PostgreSQL the transaction takes an advisory lock keyed on the remote
domain, so the joins of one domain are counted one at a time and its
per-domain and per-account bounds hold exactly, while a room's bound can
be exceeded briefly by joins of different domains that commit together.
The deployment's total is read from a count each replica takes at most
every 5 s and adjusts for its own joins and ends, so the table is not
counted for every join; the total is held to within what the replicas
seat in that time.

**How many remote domains a replica seats (G20).** A pair's queue is
opened for a join within the bound on queues remote senders open (half
the pair queues: 32 by default), and a pair keeps its queue while the
replica seats an occupant of its remote domain, so a seat's traffic never
finds every queue taken by pairs that were only idle. One replica
therefore seats occupants of at most 32 remote domains at once with the
default of 64 pair queues, and of at most half of `[limits]
max_pair_queues` otherwise (ceiling 256). A join for a further domain is
refused with `resource-constraint`. The other half stays for what local
entities send. The connection manager's own bounds are lower today: 12
remote domains with an established stream and 16 outbound sockets on a
replica ([connection manager](s2s-connection-manager-2047.md)), so those
decide first until they are raised. Before D1's default moves to `open`,
G20's evidence must cover this share: the first domains to join hold the
queues remote senders may open for as long as they keep an occupant
seated.

A stream whose stanza for the MUC service finds its domain's rate spent,
or the queues of the replica, of a worker or its domain's share of a
worker's full, stops reading until it fits, and is closed with
`resource-constraint` if it does not fit in time, as for
[outbound MUC](#outbound-muc-w6b). A stanza for a room whose own share of
the queues, or of the placement stage, is full holds no stream: it is
answered as a refused stanza of its kind is (below), and the stream goes
on with the other rooms of the domain. A leave and a delivery error are
not counted against their room's share, so they are never refused for
it.

A stanza beyond an allowance of its account is not taken at all: the
domain's rate is not spent on it and its stream is not held, so one
account cannot spend what the other accounts of its domain may send, nor
hold the stream they share. Every allowance of the account is decided
before the domain's rate, and a stanza beyond one is answered or dropped
at once; of the allowances, only the domain's rate, which all its
accounts spend together, makes a stream wait. The queues behind them
still do while they, or the domain's share of them, are full, and one
account's stanzas within its allowances, spread over several rooms whose
actors are slow, can fill the domain's share of a worker's queue: the
stream then waits for that account as it waits for the domain (below,
under risks). An account has three allowances. Requests,
messages, chat states, markers and changes of availability or nickname
count against its share. Available presence to an occupant JID counts
against its share of presence: joins, which count against nothing else,
so that an account's other stanzas never cost it a join, and changes of
availability or nickname. Leaves, errors (delivery errors among them) and
presence the host ignores count against neither share but against an
allowance of their own, at the rate and burst of the first share: beyond
it they are dropped, except the first leave its sender sends for a seat
this replica holds, which is always taken, once for each seat. A join is presence that carries the
bare `muc` element (XEP-0045 §7.2.1), or, on a replica that decides every
room itself, presence from a sender that holds no seat in the room; a
replica that carries requests to other replicas cannot tell whether the
sender holds a seat on the room's holder, so there presence without the
element is taken for a change.

A refused request or message with content is answered with
`resource-constraint` (type `wait`) within an allowance of such answers
for the domain, and so is a request for a room whose share of the queues
is full; a chat state, a marker and a change of presence are dropped
unanswered, as is a request beyond that allowance. A headline, and a
message of any type that carries only a chat state or a marker, is never
answered with an error, by the router or the host: what is not taken of
it is dropped (RFC 6121 §8.5.2.2.1 for headlines). A join beyond its
account's share of presence is answered with error presence
(`resource-constraint`, type `wait`, XEP-0045 §7.2) within an allowance
of such answers for the account (one for ten joins of the share's rate:
six at once and one every ten seconds by default), and dropped once that
is spent too; the client sees no answer to that join and retries it
later. An answer still needs a place on the queue of its pair: one that
finds none is lost with the pair's other traffic. Requests for one room
are decided one at a time, in the order they arrived.

One item of the workers that hand requests to the room host takes at
most 5 s: deciding a request, opening or changing its seat, asking the
room again for presence that enters again, and waiting for a place for
the answer share one deadline, and a seat's end is handed to the workers
that end seats. A room actor that is slow costs its shard at most that
for each of its requests, a sixth of the time a stream waits, and the
requests that wait for it fill only the room's share, which is at most
half of its domain's share of a worker or of the placement stage: no
stream is held for one room, and so none is closed and no pair's seats
are lost for it. Only a worker's queue, or a domain's share of it, filled
by more than one slow room holds a stream. The room host's own asks of a
room actor wait at most 2 s, so a room that does not answer in time is
answered for as one that cannot say, within that budget.

**Not enabled for remote principals.**

- Registration, vCards, pins, voice requests and call signalling: refused
  with `service-unavailable` before they reach a handler.
- Private messages to and from remote occupants and mediated invitations.
- In a remote occupant's message: this deployment's own payloads,
  mentions, references and processing hints. A mention therefore notifies
  nobody, and the channel-wide mention, which a managed room decides for
  its sender without stream evidence, is never reached.
- Listing rooms or occupants to remote askers.
- Creating a room.

## Spaces (W7a)

Accounts at admitted remote domains use the Spaces of this deployment that
are open to other servers, and local accounts use Spaces hosted elsewhere
(XEP-0503 version 0.3.0 over XEP-0060, with XEP-0030, XEP-0199 and the
XEP-0402 items a Space lists, pinned at
4132c531487fe9450ae90b905b8df950d0d478b2; ADR-0020 D4 and D10). The
router's part is `route/reach.rs` (which requests reach the host),
`route/local_spaces.rs` (what the router hands the host and expects back)
and `route/router/spaces.rs` (rates, queueing and the egress of event
notifications). The host is `server/routes/websocket/s2s_spaces.rs`; the
stores are in `db/s2s_spaces.rs`.

A Space is a PubSub node in the database every replica shares, so the
replica a stream lands on decides a request itself. Nothing is carried to
another replica, no relay message is added, and the ordered relay's
envelope and its id are unchanged.

### Outbound: a Space hosted elsewhere

A local account uses a remote Space with plain XEP-0030 and XEP-0060
requests to the remote service's JID, which the router routes like any IQ
request to a remote entity ([IQ correlation](#iq-correlation-g8)): the
service's list of Spaces (`disco#items`), a Space's metadata (`disco#info`
of its node), its content (`<items/>`) and joining it (`<subscribe/>`).
Nothing in this path is specific to Spaces, so a Space on a stock PubSub
service is used the same way.

The Space's event notifications reach the account through the gate of
[the account-traffic design](s2s-account-traffic-2047.md#event-notifications-xep-0060-71-xep-0163):
only for a node the account subscribed to, recorded when its
`<subscribe/>` left.

**A subscription is not a room grant.** Subscribing to a remote Space
writes one record here, the one that admits the Space's notifications. It
writes no occupancy of any room, no bookmark, no roster entry and no
permission relationship, and it changes nothing about what the account may
send. A room a Space lists is entered by a join of that room: the join is
held until this server has verified the room's service, as for any remote
room ([Outbound MUC](#outbound-muc-w6b)), and the room's own host decides
it. Nothing is inherited from a Space to its rooms in either direction
(ADR-0020 D4).

A Space's avatar and banner items (0.3.0, "Spaces images") carry URLs. The
server relays such an item to the client that asked, as it relays any item,
and requests nothing the item names.

### Inbound: the Spaces of this deployment

**Which Spaces are reached.** A Space is reached by remote principals only
while its `federated` setting is on. The setting is off for every Space
that existed before it did and for every new one; there is no deployment
default that turns it on. A Space's owner reads and sets it in the node's
configuration form (`urn:waddle:spaces:federated`, a boolean field of the
`pubsub#owner` configure form on the Spaces service). It is stored apart
from the PubSub node (`s2s_federated_spaces`, migration 2011), as a row
that exists while the setting is on and names the creation time of the node
it was turned on for: a Space deleted and created again under the same name
is not federated, whatever row its predecessor left. Deleting a Space
through PubSub or the admin command removes the row.

The setting is kept out of the PubSub node's own row because that table
belongs to the PubSub storage, which creates and verifies its own schema
and may live in a database of its own.

**The order of decisions.** Each request is decided in this order, and
nothing later is looked at before the earlier step has passed:

1. *Peer admission* (D1), for the sender's domain under the S2S snapshot
   the replica runs (`PrincipalPolicy::witness`), where the request is
   decided: a domain that snapshot does not admit, or a stream below what
   host policy requires, is refused with `forbidden`, whatever a Space
   holds for the account.
2. *The address.* The service is its bare domain. A request to any other
   address at that domain is `service-unavailable`, whatever it names.
3. *What the request itself says.* A request that is not well formed is
   `bad-request`; a `<subscribe/>` for an address that is not the sender's
   own is `bad-request` with `invalid-jid` semantics (XEP-0060 §6.1.3.1),
   an `<unsubscribe/>` for one is `forbidden` (§6.2.3.3), and a request to
   create a node is `forbidden`. These are decided before any node is
   looked at, so they are the same for every node name.
4. *Whether the node is a Space open to this asker.* The `federated`
   setting is read first, by the node's name alone. Then: no enrolled
   host-grant scope owns the node; the node exists and is the one the
   setting was turned on for; and the Space is public, or the asker holds
   an affiliation in it that is honoured for its stream and admits it.
5. *The operation*, for an open Space.

**No existence leak.** A node that fails step 4 is answered with
`item-not-found` (type `cancel`, one fixed text, no application condition)
in every operation: a node that does not exist, a Space whose setting is
off, the Space of an enrolled scope, and a private Space in which the asker
holds no honoured affiliation. The session handlers answer several of these
cases differently (`not-subscribed` for an `<unsubscribe/>`, `forbidden`
for an owner request); none of them is reached before step 4. `disco#items`
of the service lists only the Spaces the asker would be answered about.
While the setting cannot be read, the answer is `internal-server-error`
(type `wait`) for every name alike; a later read that fails is reached only
for a name whose setting is on.

**Standing (D8).** A PubSub affiliation of a remote principal is honoured
as the effect boundary honours any standing (`honoured` in the host):

- a ban (`outcast`) always stands;
- a membership counts while the principal's grants are honoured at all;
- the owner and publisher affiliations, which administer a Space and place
  rooms in it, count only for a trusted home on a `pkix` stream, under a
  private CA where the home's entry requires one. One that is not honoured
  contributes nothing: not the membership it would otherwise imply.

**What the host answers.**

| Operation | Answer |
| --- | --- |
| `disco#info` of the service | The identity `pubsub`/`service` and the features `disco#info`, `disco#items` and `http://jabber.org/protocol/pubsub`. The further features XEP-0503 has a Spaces service advertise are not told to remote askers until their use across servers has been shown (gate G21) |
| `disco#info` of a node | The identity `pubsub`/`leaf` and a `pubsub#meta-data` form with `pubsub#type` (`urn:xmpp:spaces:0`, which is how XEP-0503 recognises a Space) and the title. The owners, the creator, the creation date, the access model and the asker's affiliation are not told: they are accounts and policy of this deployment. `forbidden` for an account banned from a public Space |
| `disco#items` of the service | The Spaces the asker would be answered about, of at most 256 federated Spaces, sorted by node and paged with XEP-0059 |
| `disco#items` of a node | The rooms the Space lists that the asker may be told of (below), as XEP-0060 §5.5 lists published items: the service's address, and the item ID, the room's JID, as the name; at most 256 of the Space's items are read, sorted by item ID and paged with XEP-0059 ([MUC fallback](#muc-fallback-w7b)). `forbidden` for an account that may not read the Space, as for `<items/>` |
| Ping of the service | An empty result |
| `<items/>` | The XEP-0402 bookmarks of the rooms the asker may be told of (below), at most 256, without the publisher's address. `forbidden` for an account that may not read the Space. The store is read and nothing is written: a session's read also publishes bookmarks a Space lacks and repairs a room's parent, and a remote principal's read does neither |
| `<subscribe/>` | For the sender's own account, named by its bare JID or one of its full JIDs: recorded in the PubSub service's subscriptions and in `s2s_space_subscriptions` with what the stream proved, within the bounds (below); `resource-constraint` (type `wait`) beyond one. The service keeps one subscription for an account, under its bare JID, as it does for sessions; the answer names that bare JID and notifications are addressed to it. A Space closed to other servers while the request was decided answers `item-not-found`, and neither record is left |
| `<unsubscribe/>` | For the sender's own account: both records removed; `unexpected-request` (type `cancel`) with the PubSub condition `<not-subscribed/>` when the account holds no subscription (XEP-0060 §6.2.3.2). Leaving only reduces what an account receives, so the records are removed even for a node that is no longer a Space open to the account; the answer is then `item-not-found`, as for every other request about such a node |
| `<publish/>`, `<retract/>` and every `pubsub#owner` request | `forbidden` unless the principal's privilege is honoured (a trusted home on a `pkix` stream); then decided by the checks a session's request meets, for the remote account as their subject (below) |
| `<create/>` | `forbidden`: no Space is created for a remote principal, since creating one makes its creator the owner |
| Anything else in the two namespaces | `feature-not-implemented` where the parser names the feature, otherwise `service-unavailable` |

**The rooms a Space lists.** A Space's items are bookmarks of rooms. A
remote principal is told of a room only where the room's own host would
describe it to that principal ([Inbound MUC](#inbound-muc-w6a)): the room is
open to other servers and is no group conversation, read as the room host
reads it for every request (`room_open_to_other_servers`: the room's actor
where this replica holds one, the channel catalog otherwise, and "cannot be
told now" for an actor that cannot be reached, which tells nothing); and a
room an enrolled host-grant scope manages is told only to a principal
its canonical grants admit to view it, decided through the evidence-naming
form of the gate (`HostGrantGate::allows_as`) on the evidence of the stream
that carried the request, behind the revocation fence's ticket. A room that
cannot be decided now is not told. Nothing else a Space node holds is
served to a remote principal: an item that is no bookmark of a room of this
deployment's MUC service is left out. The same rule decides which event
notifications a remote subscriber receives.

Being told of a room admits nothing in it. Entering it is a join of the
room, decided by the room for the principal and its stream.

**Changes to a Space.** A remote principal changes a Space only with
honoured privilege, and the request then runs through the handlers a
session's request runs through (`spaces_change_for_remote`):

- The `pubsub#owner` requests (the configuration form and its change,
  purge, deletion, the affiliations and the subscriptions) are decided by
  the node's Owner affiliation. They are made without a session, so they
  neither read nor change whether the Space is federated. A trusted home's
  owner holds every other power a session's owner holds through PubSub
  ([what a remote owner of a Space can do](#what-a-remote-owner-of-a-space-can-do)).
- A publish or a retract places a room in the Space or takes it out, which
  moves the room's parent. It is decided by the Space's `owner`
  relationship or the deployment's right to create Spaces, and by authority
  over the room, for the remote account as the subject of those checks. No
  writer issues the Space `owner` relationship to a remote account, so a
  remote principal's PubSub affiliation alone places no room: the request
  is `forbidden`.
- An affiliation change is subject to D8 issuance whoever asks (below).
- An owner's change of subscriptions may record an address at another
  server. Nothing is sent for it (below).

**D8 at issuance.** A privileged standing on a Space is written only for an
account of this deployment or of a trusted home of the snapshot the replica
runs; a standing written for any other home would not be honoured at the
effect either. Three writers apply it:

- the PubSub `<affiliations/>` change on the Spaces service refuses an
  Owner, publisher or publish-only affiliation for any other account with
  `not-allowed`, before any change of the request is applied;
- the admin command `spaces:set-role` refuses the owner and admin roles
  (the node's Owner and Publisher affiliations) the same way;
- the permission actor refuses to write the channel and Space `owner` and
  `admin`, the channel `manager` and `moderator`, the Space `moderator` and
  the role `manager` relations for any other account
  (`PermissionError::UntrustedPrivilegedSubject`), in each of its three
  messages that write a relation. No handler, command or repair path names
  the last four, so the actor is the one place that decides their
  issuance; the owner and admin relations have writers that decide it
  themselves, and the actor refuses them as well, so that a writer that
  omits or misreads its own check still issues nothing. A replica attaches
  the policy at startup (`with_replica_privileged_issuance`, which the
  test-support seam `permission_actor_as_startup_builds` calls); an actor
  without one, as most suites build, asks nothing. A deployment whose
  account domain is not canonical attaches a policy that admits no
  account, so these relations are then written for nobody, a Space's
  creator included.

Membership, a ban and the removal of a standing are written for any
account.

#### What a remote owner of a Space can do

An account of a trusted home that holds a Space's Owner affiliation, on a
`pkix` stream, holds every power a session's owner holds through PubSub
(ADR-0020 D8): it reads and changes the configuration form (the
`federated` field aside, which only a session reads or changes), purges
the Space's items, deletes the Space, and reads and changes its
affiliations, under D8 issuance, and its subscriptions. It creates no
Space, and places no room in one: a publish is decided by the Space's
`owner` relationship, which no writer issues to a remote account.

- **Deleting a Space through PubSub takes its rooms out of it and destroys
  none**, for a remote owner as for a session
  (`admin::spaces::delete_space`). Before anything changes, a Space, or a
  room it names, of an enrolled host-grant scope refuses the whole
  deletion with `forbidden`. Then, under the Space's deletion lock (below):
  the Space is closed to other servers (its `federated` row and its remote
  subscription rows, in one transaction, and then the service's
  subscription records of accounts at other servers); every room the
  Space's links or items name loses its parent grant on the Space and its
  link to it; the items, which are the rooms' bookmarks in the Space, are
  retracted; the Space's own relations (`owner`, `admin`, `moderator`,
  `member`) are removed; the node is purged and deleted; its metadata is
  removed. The rooms stay, each a standalone room with its catalog row,
  its occupants and its own grants. A Space created again under the name
  inherits none of it. A failure part-way puts back the links, parent
  grants and items it removed and answers `internal-server-error`, and
  leaves the Space closed to other servers, which its owner turns on again
  in a session; a failure after the node is gone is completed by
  `spaces:delete`, since the PubSub request then answers `item-not-found`.
- **Only the admin command `spaces:delete` destroys rooms.** It runs the
  same deletion, and destroys, with its catalog row, every room whose
  parent the Space was: its parent grant on the Space is found and removed
  by the deletion, no link names another Space, and an item under the
  room's canonical address names it. A room an item or a link names
  without that parent grant, and a room of another MUC service, is only
  taken out of the Space.
- **Two deletions of one Space never interleave.** Each runs under a lock
  of the Space's deletion: a transaction-scoped advisory lock of its own
  on PostgreSQL, held for the whole deletion, and a lock in the process on
  SQLite, which one process serves. A deletion that fails, and puts back
  what it removed, therefore never does so beside one that completed; a
  deletion that waited reads the node again and answers `item-not-found`
  when the other deleted it.
- **Purging a Space's items** takes the rooms they name out of the Space
  first (each loses its parent grant on the Space and its link to it, as a
  retract does); the rooms stay. A purge leaves nothing of S2S behind: the
  `federated` setting and the remote subscriptions name the node, which
  remains.
- A remote owner can empty and delete a Space of this deployment, which
  leaves its rooms standing alone. Listing a home as trusted is the
  operator's decision to give its accounts that power over the Spaces they
  are made owners of.

**Recovering a Space from a remote owner.** These are server-administrator
steps; nothing a remote owner can do reaches them.

1. Take the standing away: the admin command `spaces:set-role` with the
   remote account as `member_jid` and the role `none` (or `member`)
   removes its Owner affiliation, which is the only privileged standing a
   remote account can hold on a Space. It takes effect at the next request.
2. Or stop honouring every owner of a home at once: remove the domain from
   `admission.trusted_homes` in the S2S configuration. The owner and
   publisher affiliations of its accounts are then honoured as nothing at
   the next request, on every replica that runs the new snapshot, and are
   written for nobody of that home.
3. Undo what the owner changed: `spaces:set-role` with the role `owner`
   gives a local owner its standing back, a ban included, and a local
   owner's configuration form restores the access model. Subscriptions the
   remote owner removed are made again by their accounts.
4. A Space deleted through PubSub is not recovered by any command; its
   rooms remain, and a local owner creates a Space again and places them
   in it (a publish of each room's bookmark). The rooms `spaces:delete`
   destroyed are recovered only by a restore from backup.

**Event notifications to remote subscribers.** A publish or a retract on a
Space fans out to its local subscribers; a subscriber at another server is
skipped there. A retract is notified once it stands: a Space node's
`pubsub#notify_retract`, which XEP-0503's node configuration requires to be
on, is the setting to notify subscribers when items are removed (XEP-0060
§17.4.4), and a request that asks with `notify` must be notified (§7.2.2.1). Once the publication is stored, the fan-out only
queues it for the Spaces host (`queue_remote_notification`, then
`RemoteRoute::space_published`): a bounded queue of 1,024 publications for
each replica. A publication takes one of its Space's allowance of notified
publications before anything is queued or read, and one beyond it is not
queued (counted by `waddle.s2s.spaces.events.limited`), so one Space holds
at most its burst of the queue's places and a busy Space cannot fill the
queue for the others. A publication that finds the queue full is not
notified (counted by `waddle.s2s.spaces.publications.dropped`). One task
of the host takes them in order and decides each for the remote
subscribers (`notify_remote_subscribers`), so a publisher's request never
waits for what is read about them. Publications still queued when the
replica's state goes away are never decided, and are counted by
`waddle.s2s.spaces.publications.discarded`. A notification leaves for an address at another
server only where all of this holds when it is handed over:

- the replica serves its Spaces to remote principals, the Space is
  federated, and its node is still the one the publication was made to;
- the remote account made the subscription itself over an inbound stream
  (a row of `s2s_space_subscriptions` bound to this node, made or made
  again within the last thirty days), and the PubSub service's own record
  still holds it as deliverable, read for this publication. A subscription an owner recorded for a remote address has no
  such row and sends nothing, so a local owner cannot aim a Space's
  notifications at an address that never asked;
- the subscriber's domain is admitted now, and its affiliation, honoured
  for the evidence its subscription recorded, still lets it read the Space;
- the room the event names is one the subscriber may be told of.

The notification is the event local subscribers get, addressed to the
subscribed account's bare JID, from the service's bare JID and naming the
one node, without the publisher's address. Whether the room is open to
other servers is read once for a publication; what is decided for each
subscriber is its standing and, for a managed room, its grants. The pair's worker checks once more before it
writes: peer admission must admit the remote domain then.

A subscription outlives the stream that made it, so its row records what
that stream proved (authentication strength, trust profile and the snapshot
it was admitted under), as a seat does. Peer admission and trusted-home
status are not stored: they are evaluated at each notification.

**What ends a remote subscription.** Its account's `<unsubscribe/>`; the
owner removing it (`<subscriptions/>` with `subscription='none'`, which
removes the row as well as the service's record); a ban of the account
(`<affiliations/>` with `outcast`), which removes the row, so lifting the
ban does not bring the subscription back; the Space being closed to other
servers or deleted; and thirty days without the account subscribing again
(below). A domain that is no longer admitted receives nothing from the
moment it is not, and its rows give way when they would keep an admitted
account out (below). Where a row goes because it lapsed or its domain is
no longer admitted, the request that removed it then removes the
service's record of that subscriber too.

A ban and a subscription being recorded at the same time cannot leave a
row behind: the ban is written before the row is removed, and the removal
takes the Space's lock; the subscription reads the account's affiliation
again once its row is committed, removes the row for a ban it finds and is
refused, `forbidden` in a public Space and `item-not-found` in a private
one, as for a banned account's request. Either the removal finds the row,
or the subscription finds the ban. The affiliation is read after the
commit, not under the Space's lock, because the PubSub store may share the
database pool: a transaction that waited for a second connection while
holding its own and the lock could stall every subscription when the pool
is taken by waiters.

Turning the `federated` setting off removes the setting's row and every
remote subscription row of the Space in one transaction, under the
Space's lock (below), and then the service's subscription records of every
address this deployment does not serve, an owner's record for one
included. A subscription being recorded at the same time re-reads the
setting under that lock and is refused when it is off; one whose service
record is written after the records were removed reads the setting once
more and removes its own. Either way no record of a remote subscriber
outlives the setting.

**Bounds.**

| Bound | Default |
| --- | --- |
| Requests one remote domain may address to the Spaces service, on average and at once | 50 a second, 200 |
| Of those, the requests of one remote account, on average and at once | 10 a second, 40 |
| Refusals of requests beyond an allowance that are answered, for one remote domain (beyond it they are dropped) | 5 a second, 20 |
| How long the host may take to decide one request | 5 s |
| Spaces listed, and items returned, in one answer | 256, 256 |
| Subscriptions accounts at other servers hold to one Space (exact) | 256 |
| Subscriptions the accounts of one remote domain hold to one Space (exact) | 32 |
| Subscriptions of one remote account (exact) | 64 |
| Subscriptions of one remote domain (exact) | 4,096 |
| Subscriptions accounts at other servers hold in the deployment (approximate on PostgreSQL, below) | 65,536 |
| How long a remote subscription holds without being made again | 30 days |
| Domains read, and rows removed, by one sweep of the whole store | 4,096, 1,024 |
| Publications awaiting the decision for remote subscribers, for each replica | 1,024 |
| Publications of one Space queued for that decision, on average and at once, for each replica | 4 a second, 16 |
| Event notifications waiting for one directional pair's stream | 256 |

A request beyond its account's share or its domain's allowance is answered
with `resource-constraint` (type `wait`) within the domain's allowance of
answered refusals and dropped beyond it; it never holds its stream, and the
account's share is decided first, so one account cannot spend what the
other accounts of its domain may ask. A request within its allowances is
queued for the host on the queues account traffic takes, keyed by its
sender's bare JID, so one account's requests are decided in order under the
same bounds (64 for one sender, 256 for a worker, 64 of a worker's 256 for
one remote domain), and a request that finds them full holds its stream
until it fits. A host that does not decide a request in time leaves it
unanswered; its work is not cut short, so a subscription may then be
recorded although its answer was dropped, and subscribing again is
answered.

The subscription bounds are counted and the row written in one
transaction. On SQLite it is an immediate transaction, which holds the
database's write lock, so every bound is exact. On PostgreSQL the
transaction takes two advisory locks, one keyed on the subscriber's domain
and then one keyed on the Space, so the subscriptions of one domain, and
those to one Space, are counted one at a time on every replica: the bounds
on a Space, on one domain in a Space, on an account and on a domain are
exact. The deployment's bound is counted under neither lock of its own, so
subscriptions of different domains to different Spaces may pass it
together; it is approximate by at most the number of such subscriptions
decided at once. Turning a Space's setting off takes the Space's lock
alone, and every transaction that takes both takes the domain's first, so
they never wait on each other in a cycle.

Each bound is counted by the database, up to the bound, so checking one
costs one row read whatever its size. A subscription the account already
holds is recorded again whatever the bounds, which renews it. Only the
subscriptions of domains admitted now hold a place against a Space's bound
and the deployment's: when either is reached, the rows of domains the
replica's snapshot does not admit are removed and the bound is counted
again. A row whose subscription was last made more than thirty days ago
has lapsed: it counts against no bound, names no subscriber, and is
removed when its Space is next subscribed to or the deployment's bound is
reached. A replica on an older snapshot may remove the rows of a domain
another replica still admits; that account then receives nothing until it
subscribes again, which is the safe side.

When the deployment's bound is reached, the subscription sweeps the whole
store: lapsed rows first, oldest first (`subscribed_at_ms` is indexed),
then the rows of domains that are not admitted, reading at most 4,096
domains and removing at most 1,024 rows in all. The domains are read from
the one after the last domain the replica's previous sweep read, wrapping
round to the first, so successive sweeps reach every domain and not only
the first 4,096 by name; the cursor is the replica's own and kept in
memory. The sweep's deletions reach rows of Spaces whose lock the
transaction does not hold, so on PostgreSQL they pass over a row another
transaction holds rather than wait for it (`FOR UPDATE SKIP LOCKED`): a
sweep never waits for a row, and so closes no cycle of waits with a
transaction counting another Space. Every other deletion of the table
reaches only the rows of the Space whose lock its transaction holds. A
sweep that does not bring the count below the bound is undone with the
rest of the refused subscription.

**Amplification.** One publication sends at most one stanza to each
subscribed account at another server, at most 256 for a Space, and only
within the Space's allowance of notified publications; a publication beyond
it reaches the local subscribers alone. A notification opens its pair's
queue only within the share of pair queues that what remote senders ask for
may open (half of them, 32 by default), holds one of the pair's 256 places
for notifications until it is written or dropped, and keeps no queue alive
afterwards: unlike a seat, a subscription pins nothing. What finds no
place, or a domain that is not admitted, is dropped and counted. A
notification is a headline and is never sent again; a subscriber that
missed one reads the Space's items. A remote principal can cause traffic to
another remote JID only by changing a Space, which needs honoured
privilege, and then only to addresses that subscribed themselves.

**Not enabled for remote principals.**

- The Space of an enrolled host-grant scope. Sessions are served nothing
  from it either: no grant of a scope is a Space's, so nobody's standing in
  it can be decided
  ([host-grant design](host-grant-authority-2047.md#not-decided-here)).
- Creating a Space, and reading or changing its `federated` setting.
- The `authorize` access model, pending subscriptions and their approval:
  Spaces are public (`open`) or private (`whitelist`).
- Subscription options, retrieval of the asker's own subscriptions and
  affiliations across the service, and the last published item on
  subscribing.
- Ad-hoc commands and everything else addressed to the Spaces domain.
- An error message a subscriber's server returns for a notification is
  dropped; it does not end the subscription (XEP-0060 §12.3 allows a
  service to).

## MUC fallback (W7b)

XEP-0503 version 0.3.0 (xsf/xeps
4132c531487fe9450ae90b905b8df950d0d478b2) keeps a Space's rooms ordinary
XEP-0045 rooms, and ADR-0020 D4 rests third-party interoperability on that:
a client without Spaces support, local or remote, reaches every child room
of a Space by its plain MUC JID. Reaching a room needs no code of its own:
a room is joined, read and discovered at its own address, decided by the
room's own rules, whichever Space lists it, and nothing is inherited from a
Space to its rooms. What this slice adds is the discovery that lets such a
client find the rooms: the MUC service's `disco#items` (XEP-0045 §6.3),
for remote askers as for sessions, and the `disco#items` of a Space's node,
which XEP-0503 ("Advertising space membership") names as the way to verify
that a room is part of a Space.

**The MUC service's rooms (XEP-0045 §6.3).** The service lists a room to
an asker when all of these hold:

- the room is public: a hidden room (`muc#roomconfig_publicroom` off) is
  listed to nobody, as §6.3 says;
- it is no group conversation of this deployment's accounts;
- for an account at another server, its `federated` setting is on, read
  from the channel catalog for a room with a catalog row, which the room's
  configuration keeps in step, and from the room's actor for an instant
  room. The room's Space adds and removes nothing: a federated room in a
  Space that is not open to other servers is listed and entered like any
  other, and a room that is not federated is not listed whatever its
  Space's setting says (D4);
- for a room an enrolled host-grant scope manages, the asker's canonical
  grants allow `view`: for a session, decided without stream evidence, as
  channel search decides it; for an account at another server, on the
  evidence of the stream that carried the request, through the
  evidence-naming form of the gate behind the revocation fence's ticket,
  as the room's own `disco#info` decides it. A room whose grants cannot be
  read now is not listed.

Any other room is absent from the listing, as a room that does not exist
is, so the listing tells nothing of it.

**How a listing is built.** A listing reads the channel catalog and the
live instant rooms. A session's listing reads every catalog row, public or
not, within 10,000 rows, so that its scan of the live rooms sets every
catalog-backed room aside in memory. A listing for accounts at other
servers reads only the rows it may list (public, open to other servers and
no group conversation, checked again in memory), within 4,096 rows, so no
hidden room, room closed to other servers or group conversation takes the
read's places; its scan then checks every live room whose address names a
channel against the catalog. A room with a catalog row is listed from its
row and never from its actor's configuration. The scan of the live rooms
takes at most 1,000 candidates; candidates whose address names a channel
are checked against the catalog, 500 to a query, whenever the read did not
name every row, and one the check cannot clear (a query failed or ran out
of time) is left out. The actors of the remaining instant rooms are asked
sixteen at a time, and a room whose actor has not answered within the
listing's reading time (1.5 s, 1.2 s for one read) is left out. Whether a
room is managed is read from the replica's enrolled set for every request,
and a managed room's grants are read four rooms at a time; every grant
that allowed `view` is confirmed with `HostGrantGate::still_allows` just
before the answer is built, the decision itself while its fence ticket is
current and a decision read again otherwise. A managed room not decided
or confirmed within the listing's time is left out: 2.5 s in all for a
remote asker, and for a session 1 s once its rooms are read, which takes
it at most 2.7 s (a read that started before it asked, and a whole read of
its own). A listing whose catalog or enrolled set cannot be read is
answered with `internal-server-error` (type `wait`).

**What a listing left out.** A listing that left a room out tells no
count (XEP-0059 §1), whatever the reason: a read that filled its bound, a
scan beyond its 1,000 candidates, a managed room whose grants could not be
read, a live room not read in time (the scan of the live rooms, an instant
room's actor or the catalog check of a live room), or a managed room not
decided in time. Such a listing carries its `<set/>` even when the request
had none. A remote asker is answered with the rooms that were listed. A
session is answered with `internal-server-error` (type `wait`) only when a
managed room was not decided or confirmed in time, rather than with a
listing that lacks it: the first-party client takes a listing as the whole
of the service's rooms and reads no `<set/>`
(`chat/src/lib/xmpp/discovery.ts`), while it takes an error as a listing
it may not rely on and keeps the rooms it knows. A live room whose actor
did not answer in time is left out of a session's listing, which is
answered with the rest and no count, so that one slow room's actor costs
its own place in the listing and never the listing. A session's listing
left short that way, by a bound or by grants that could not be read is
answered with the rest and no count; the first-party client, which reads
no `<set/>`, still takes such a listing as complete, so a slow instant
room is missing from that client's list until a later listing reads it.

**Shared reads.** What the catalog and the rooms' actors say of the rooms
(not who manages them, and not anyone's grants) is read at most once at a
time on a replica for each audience, and every asker that waits for that
read is answered from it. The read runs, within 1.2 s, in the task of the
asker that started it; when that task ends first (its router stops, or its
session closes), the askers that waited for it ask again and one of them
starts the next. An account at another server is answered from a read
that started at most 2 s before, so a room whose catalog row or
configuration changed, a room closed to other servers included, can be
listed as it was for up to 2 s. A session is answered only from a read
that started once its request arrived, waiting for the read running now
to end if that one started earlier, so a change the session made before
it asked is in its answer. An asker that waited for a read which ran out
of the time of the asker that started it asks again.

A request of an account at another server for the service's rooms is
handed by the room shard that took it to a task of its own, so no room's
requests wait behind a listing. A replica builds at most four such
listings at once, at most two of them for one remote domain, and answers a
request beyond either with `resource-constraint` (type `wait`). The host
answers within 2.5 s; the router answers a listing the host did not answer
within the per-item budget (5 s) with `internal-server-error` (type
`wait`), so the request is always answered. An instant room, which has no
catalog row, is known only to the replica that holds its actor, so a
listing built on another replica lacks it, and the pages of one listing
built on different replicas can differ; a catalogued room is listed the
same by every replica.

XEP-0045 §6.3 says a service SHOULD return the full list of the public
rooms it hosts, and the rooms an account at another server may reach are
the public ones open to other servers, so the listing follows the SHOULD
for those: listing them tells an asker nothing a `disco#info` of each room
would not. A managed room is listed to an account that may view it, which
is what the room's `disco#info` tells that account
([host-grant design](host-grant-authority-2047.md#b2-policy-for-the-surfaces-this-slice-protects-g14-part)).

**A Space's rooms.** The `disco#items` of a Space's node lists the rooms the
Space holds, each as XEP-0060 §5.5 lists a published item: the service's
address as `jid`, the item ID, which is the room's JID, as `name`, and no
`node`. A client verifies that a room it was told of is part of the Space
by finding the room's JID there. Only bookmarks of rooms are listed: an item
that is no XEP-0402 `conference`, such as a Space's avatar or banner, is
left out.

- A session is answered for a Space it may read (as its `<items/>` read
  is decided) and gets `item-not-found` for any other node, a node that
  does not exist and the Space of an enrolled scope included, in the same
  error from the same address; `internal-server-error` (type `wait`)
  while that cannot be told. Every room bookmark is listed, as the Space's
  items show it, except a room an enrolled scope manages that the
  session's grants do not allow it to view, decided four rooms at a time
  and confirmed before the answer as for the MUC service; when a room is
  not decided within 2.5 s the session is answered with
  `internal-server-error` (type `wait`), and a listing that left out a
  room whose grants could not be read tells no count. An item without a
  payload is no room.
  One answer lists at most 1,024 rooms; a listing that does not fit
  carries its `<set/>` and the rest is reached by paging. Unlike a read of
  the items, the listing writes nothing: it does not publish the
  bookmarks a Space lacks or repair a room's parent.
- An account at another server is answered as for every request about a
  node ([Spaces](#spaces-w7a)), and is told of the rooms it may be told of
  there: rooms of this deployment's MUC service that are open to other
  servers, and, for a managed room, that its grants admit it to view. The
  Space's 256 most recently published items are read, as a read of its
  items reads them: a room among older items of a Space that holds more is
  not listed to accounts at other servers, and such a listing carries its
  `<set/>` with no count, since what the read left out is not known. A
  room whose openness to other servers or whose grants cannot be read now
  is left out, and that listing tells no count either. The
  Spaces service's own listing tells no count either when more than 256
  Spaces are open to other servers, or when a Space could not be decided
  now and was left out.

**Paging (XEP-0059).** Every listing above is sorted by its key (the room's
JID for the MUC service, the item ID for a Space's rooms, the node for the
Spaces service) and answers a `<set/>` request: `max`, `after`, `before`
(an empty one for the last page) and `index`. An `after` or `before` anchor
is placed by its order and never looked up, so an anchor that names a room
the asker is not listed tells nothing of it, and a room removed between
pages skips nothing. The count and the first item's index are of the
listing as built for the asker, and the count is told only for a listing
that left nothing out. A remote asker gets at most 256 items in
one answer whatever `max` asks, and a session at most 1,024 of a Space
node's rooms; a listing that does not fit in one answer carries the
`<set/>` even when the request had none, as a responder that returns a
limited result set does (XEP-0059 §1, XEP-0045 §6.3). A session's listing
of the MUC service's rooms is not bounded per answer. XEP-0059 is read at
the same pinned commit. A `<set/>` that is not well formed is
`bad-request`.

**Evidence.** Written, compiled and run at `e559ebd57`, every case passing
(the run10 head logs of the [verification checklist](../../../docs/evidence/2047/unrun-tests.md)):

- `crates/waddle-server/tests/xep0030_s2s_inbound_muc_discovery.rs`: the
  service lists its open rooms and a room no occupants; no room a remote
  asker may not reach (not federated, hidden, a group conversation) while
  a session is still told of the rooms its rules list; paging, with the
  count of the asker's listing and an anchor placed by its order; backward
  paging and an index; an anchor naming a hidden room, or one not open to
  other servers, answered exactly as one naming no room; a listing of 257
  rooms answered with 256 and an unasked `<set/>`; rows of hidden rooms,
  rooms closed to other servers and group conversations that take no place
  in the remote read; a remote read that filled its 4,096 rows told no
  count; a live room whose actor does not answer in time left out of a
  remote listing and of a session's listing that both tell no count, two
  remote listings asked at once built from one read, and both naming the
  room once the actor answers; live rooms whose catalog rows are hidden or closed to other
  servers not listed to a remote asker, though their actors say open; and
  a client that walks from the server's items to the service's, finds a
  Space's room and enters it by its plain JID while the Space is not open
  to other servers;
- `crates/waddle-server/tests/xep0045_s2s_inbound_muc.rs`: a managed room
  listed only to a remote member its grants admit, and to nobody once it
  is closed to other servers and the reuse window has passed
  (PostgreSQL);
- `crates/waddle-server/tests/xep0045_s2s_inbound_muc_router.rs`: room
  lists the host does not answer in time answered with
  `internal-server-error` (type `wait`), those beyond one domain's two
  places with `resource-constraint` while another domain's lists take the
  remaining two, a room's request not held behind them, a room's IQ the
  host did not decide in time answered and a seated occupant's change of
  presence not; a message the host took and did not answer in time left
  unanswered and an IQ it took told that it may have been applied, while
  a message it had not taken is answered with an error; and every request
  of a slow room answered (scripted host);
- `crates/waddle-server/tests/xep0503_s2s_inbound_spaces.rs`: a Space's
  node lists the rooms a remote asker may be told of, in pages, refuses
  a banned account, tells no count once the Space holds more than the
  256 items read, and tells no count while a room's openness to other
  servers cannot be read;
- `crates/waddle-server/tests/xep0503_s2s_inbound_spaces_host_grant.rs`: a
  managed room in a Space's node listing only for the member (PostgreSQL);
- `crates/waddle-server/tests/xep0503_spaces_ws.rs`: a Space's node lists
  its room to a session, and a session that never asks the Spaces service
  finds the room in the MUC service's listing and enters it by its plain
  JID; a Space the session may not read answered as a node that does not
  exist; a session's paging of a Space node's rooms;
- `crates/waddle-server/tests/xep0045_host_grant_fence.rs`
  (`host_grant_fence_cases/sinks.rs`): the managed room listed to a member
  and not to an ungranted account, and the enrolled Space node's children
  not found; a Space no scope enrolls listing the managed room only to a
  member, and no item without a payload; a managed room whose grants
  cannot be read left out of both listings, which are still answered and
  tell no count (PostgreSQL);
- `crates/waddle-server/tests/host_grant_remote_principals.rs`: a stale
  `view` decision confirmed against the grants read again, refusing an
  account revoked since and allowing one that holds its grant
  (PostgreSQL).

## Behaviour with S2S off

Outbound MUC changes nothing while S2S is disabled: the remote route still
refuses a remote room's join with `remote-server-not-found`, and the leave
when a resource ends, like the adoption when a session resumes, is a
registry lookup and a call that does nothing. Received private messages
carrying the `muc#user` element from a full JID outside the account
domain are no longer received-carbon-copied (XEP-0280 §6.1), whether or
not S2S is on.

Inbound MUC changes nothing for remote senders while S2S is disabled: no
router exists, so no room host is attached, no delivery hook is installed
in the connection registry, no request is carried between replicas, and
every join, read and administration request of a local session is decided
without stream evidence as before. Several of its changes apply whether or
not S2S is on, and are listed below with the others.

Spaces change nothing for remote senders while S2S is disabled: no router
exists, so no Spaces host is attached, the pass that notifies subscribers
at other servers returns before it reads anything, and migrations 2011 and
2012 create two tables that stay empty unless an owner turns a Space's
`federated` setting on. The
setting can be set while S2S is off and takes effect once it is on. Two of
its changes apply whether or not S2S is on, and are listed below.

The MUC fallback changes nothing for remote senders while S2S is
disabled: no room or Spaces host is attached, so no listing is built for
one. Three of its changes apply whether or not S2S is on, and are listed
below.

These change behaviour while S2S is disabled, and belong to the rollout
runbook (G24):

- The MUC service's `disco#items` lists a public room an enrolled
  host-grant scope manages to a session whose canonical grants allow
  `view`, as the room's own `disco#info` tells that session; such a room
  was listed to nobody before this change. The first-party chat client
  joins every room the MUC
  service lists that it holds no bookmark for
  (`chat/src/lib/xmpp/discovery.ts`, where a listed room without a
  bookmark is given `autojoin`), so at connect a member now joins each
  managed room it may view, as it already joins every public unmanaged
  room. Every connect-time listing of a session reads the grants of each
  public managed room in the catalog, four at a time within the listing's
  2.5 s; a session's listing whose catalog or enrolled set cannot be read
  is answered with `internal-server-error`, as it was.
- A session's listing of the MUC service asks the actors of live instant
  rooms sixteen at a time within one read of 1.2 s, where it asked them
  one after another with 5 s each. An instant room whose actor is slower
  is left out, as before, and the listing tells no count. When a managed
  room's grants are not decided within 1 s of the read the session is
  answered with `internal-server-error` (type `wait`); before, it was
  answered with the rest of the rooms. A
  listing left short by a bound, by a live room not read in time, or by a
  managed room whose grants could not be read, tells no count where it
  told the count of what it listed.
  When the catalog read fills its bound, an instant-room candidate whose
  address names a channel and that the catalog check cannot clear is left
  out; before, it was kept and listed from its actor's configuration.
- Sessions' listings of the MUC service share their reads of the catalog
  and the live rooms: a listing is answered from the read running when it
  arrived only if that read started after it arrived, and otherwise waits
  for the next one, so many listings at once cost one read at a time on a
  replica and each still sees the changes made before it was asked.
- The `disco#items` of a Space node answers a session with the rooms the
  Space holds (the service's address and each room's JID as the item's
  name), at most 1,024 in one answer, or `item-not-found` for a node the
  session may not read; before, it answered an empty list for any node.
  First-party clients read a Space's rooms through `<items/>`, which is
  unchanged.

- A Space owner's configuration form on the Spaces service carries one
  more field, `urn:waddle:spaces:federated`, off by default.
- A privileged standing on a Space is no longer written for an account at
  a domain the deployment does not serve: `spaces:set-role` refuses the
  owner and admin roles, the PubSub `<affiliations/>` change refuses the
  owner, publisher and publish-only affiliations (`not-allowed`), and the
  permission actor refuses the channel `manager` and `moderator`, the Space
  `moderator` and the role `manager` relations. With S2S off no home is
  trusted, so this holds for every such account; standings stored before
  stay as they are and are honoured for nobody over S2S.
- A Space's event fan-out no longer looks up connections for a subscriber
  at a domain the deployment does not serve; it found none before.

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
- Migration 2007 creates `s2s_local_room_seats` on both engines, and the
  owner phase grants the runtime `SELECT, INSERT, UPDATE, DELETE` on it. It
  stays empty while S2S is off.
- In a room no host-grant scope manages, a `muc#admin` request that grants
  an owner or admin affiliation to an account outside the domains this
  deployment serves is answered `not-allowed`, unless the account's home is
  a trusted home of the S2S snapshot; with S2S off there is none. Before,
  the room recorded such an affiliation for any JID. Affiliations already
  recorded are kept, and are honoured for a remote principal only as D8
  allows. The moderator role is refused the same way for an occupant whose
  seat does not honour room moderation; with S2S off every occupant is a
  local session, which it always does.
- The admin command that sets a room affiliation
  (`channels:set-affiliation`) refuses an owner or admin affiliation for
  such an account the same way. Before, it wrote one for any JID.
- Migration 2008 adds `channels.federated` on both engines, zero for every
  row. The owner's configuration form of every room offers a new boolean
  field, `urn:waddle:roomconfig:federated`, off for every room; setting it
  has no effect while S2S is off, since no remote principal reaches a room.
  A configuration change that turns it on or off is announced to the
  room's occupants as any other change that is not a privacy change
  (status 104). `[host_policy] federate_new_rooms` is a new key of the S2S
  snapshot, which is not read while S2S is off; a binary that predates it
  refuses a snapshot that holds it, so replicas run the new image before
  the snapshot uses the key, and a rollback removes it first.
- The inbox and activity projections write only for an address at a domain
  the deployment serves. Before, a room's durable member at another domain
  got an inbox row for each message, and a join, a leave, a chat state or a
  displayed marker of an occupant at another domain recorded activity; with
  S2S off no session can seat such an occupant, so only a membership
  recorded for an account of another domain is affected, and nothing read
  those rows. Rows written before are left as they are.
- A correction (XEP-0308) or retraction (XEP-0424) of a room message is
  accepted only from the account the archived message records as its
  author, where it records one. Before, the room compared occupant
  addresses only, so an account that entered under the nickname of another
  account's archived message could retract it. An account revising its own
  messages sees no change.
- `waddle.clustering.relay.remote_room_request.v1` is a new relay message
  id. Nothing sends it while S2S is off.
- In a room an enrolled scope manages, the revocation sweep removes a
  seated occupant of a domain peer admission does not admit, which with
  S2S off is every domain the deployment does not serve
  ([Seated occupants](host-grant-authority-2047.md#seated-occupants-roles-and-media-g14)).
  Before, such an occupant kept its seat until its grants denied it. With
  S2S off no session can seat an occupant of another domain, so the change
  is visible only to an occupancy recorded while S2S was on.
- With clustering, a replica that takes a room's claim seats its recorded
  occupants again. A recorded occupant at a domain the deployment does not
  serve is seated again only if the replica takes over its seat, which
  needs S2S; with S2S off it is left out, in every room. Before, it was
  left out of a managed room and seated again in any other, where nothing
  could reach it.

## Restrictions and risks

- **Cross-replica latency of a held join.** A join waits for its
  service's and its room's verification on the replica that routed it.
  When a `disco#info` response reaches another replica, the join moves on
  at the routing replica's next sweep, up to five seconds later, once for
  the service and once for the room.
- **Bounds under concurrency.** On PostgreSQL the deployment's total of
  occupancies can be exceeded by the joins of different accounts that
  commit at the same moment; the per-resource and per-account bounds hold.
  Verifications are bounded per replica (256 services and rooms), not per
  account, so one account joining many rooms not asked before at once,
  such as a client's bookmarks autojoining after it connects, can hold all
  of them and delay other joins with `resource-constraint` for up to the
  verification timeout. Each such room costs a verification for the room
  as well as, once, one for its service.
- **A waiting stream holds back its other rooms.** A stream that waits
  for its stanza to fit on the room delivery queues reads nothing else
  meanwhile, so the peer's traffic for other rooms and other occupants on
  that stream waits too, and IQ responses on it arrive late. Each stanza's
  wait ends within 30 seconds, but a stream may wait again for each later
  stanza that finds the queues full; a stanza that does not fit in time
  closes the stream, and its occupants are told to join again.
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
  answers `disco#info` for a domain as a conference service and for a JID
  there with the conference identity or `item-not-found`, wrongly or
  deliberately, has that JID treated as a room: the join is sent and its
  traffic admitted. Verification shows that the remote server presents
  the JID as a room at a multi-user chat service; a server that hosts
  accounts on its own domain names a `server` identity there and its
  accounts are refused, and an account at a service domain that answers
  as an account is refused too.
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
- **Room traffic and the sender's blocking list.** With S2S enabled, a
  client's message or presence to a remote JID on its own XEP-0191 list,
  a remote room or occupant included, is answered with `not-acceptable`
  and `<blocked/>` and not routed (account-traffic slice); inbound room
  traffic passes the recipient's list.
- **An address outside the identity profile in room presence.** A room
  that names an occupant's real JID (`<item jid='…'/>`) in a spelling the
  identity profile refuses has its presence dropped unanswered (gate
  G10), so the occupant does not see it; nothing is sent back to the room.
  This is visible only with third-party peers.
- **A room's self-ping reflection** reaches the client once the account
  side is attached (W6c); a router without one answers it
  `service-unavailable` itself.

- **A request arrives on one replica and is answered from another.** A
  stanza for a room another replica holds is carried to that replica, which
  answers on its own stream to the occupant's domain. A peer that keeps one
  stream per directional pair, as Waddle does, then sees this deployment's
  stream to it come from the holder; when two replicas hold rooms with
  occupants at one domain, or the carrying replica answers a refusal while
  the holder sends traffic, both dial that peer and alternate as described
  under "Several replicas, one stream per pair": a seat whose stanza cannot
  be handed over within the dial's budget ends with status 333. Verification
  with two replicas (steps 6 and 8) decides whether a pair's egress must be
  routed through one replica before a pilot.
- **A carried request's hand-over can have an unknown outcome.** When the
  relay's reply is lost after the holder queued the stanza, the carrying
  replica answers the sender with a retryable error and the holder still
  decides the stanza: a join can be both refused by one replica and
  answered by the other. Nothing is decided twice, since the carrying
  replica never asks again after a failure that may have reached a handler,
  and a client that retries the join finds its seat and is answered as for
  a change of availability.
- **Order across a moving claim.** Requests for one room are carried one at
  a time by the replica they reach and decided by the holder in the order
  it queued them. While a room's claim moves, a request can reach the old
  holder, which refuses it, or wait for the new one; a room whose claim
  moves keeps its remote occupants only if the new holder takes over their
  seats, which it does once the previous owner is no longer alive, and
  otherwise they are removed and join again.
- **The relay carries the stream's facts on the cluster's word.** The
  holder cannot verify the stream another replica verified. It trusts the
  carried facts as every relay message is trusted: only replicas of this
  deployment reach the relay. It decides admission and standing itself,
  from its own snapshot, so a carrying replica with an older snapshot
  cannot admit what the holder's snapshot refuses, but a compromised
  replica could assert evidence for any remote address, as it could
  already assert any session's.
- **Federated rooms are open to every admitted domain.** A federated room
  that is not members-only admits any account of any admitted domain as a
  participant and serves it the room's archive, without the account being
  known to this deployment. With D1 in `allowlist` mode that is the listed
  peers; under `open` it is every domain that authenticates. A room is
  federated only when its owner, or the deployment's default for new
  rooms, says so, and the setting is one for all admitted domains: it does
  not name which.
- **An archive names its authors.** The rooms are non-anonymous: a remote
  asker that may read a federated room's archive receives each message
  with its sender's real JID, as a local reader does.
- **The setting is read where the room is.** A room's `federated` setting
  is read from its actor, or from the channel catalog while no replica
  holds one. A room without a catalog row whose actor no replica holds (a
  room its first occupant created, after every replica restarted) is not
  found until a local occupant enters it again.
- **A room without a catalog row can be told from a missing one while its
  holder cannot take requests.** A room its first occupant created has no
  catalog row, so a replica that does not hold it cannot read its setting
  and carries a request for it. While the holder is busy, out of reach,
  moving or predates the relay message, the asker receives the retryable
  error where a room that does not exist answers `item-not-found`, whether
  or not the room is federated. Rooms with a catalog row are answered
  where the request arrived and do not differ. The same holds on one
  replica while a room's setting cannot be read (`internal-server-error`).
- **The deployment default applies where a room is created by a request.**
  `federate_new_rooms` sets the setting of a room its first occupant
  creates and of a channel the channel-creation command creates. Channels
  made any other way (the topology bootstrap, a Space's own channels)
  start with the setting off.
- **What an admitted domain can make this deployment store.** Its
  accounts' messages are archived by the federated rooms that accept them,
  at most at the domain's request rate and, for each account, at the
  account's archive allowance. The seat bounds are per room, per domain and
  per account; the request rate is per domain, so one admitted domain can
  use a room's 256 seats and its own 4,096 with accounts it mints, each
  with an allowance of its own. The allowances are kept in memory on the
  replica that holds the room, so they start full after a restart or a
  claim move.
- **An administration request's answer is settled at hand-off.** The
  room's effect records for a remote requester's own copies are settled
  when its answer is handed to the router's queue, not when a stream wrote
  it. An answer the queue then drops is not delivered again; the requester
  sees the result of its request at its next presence or list request.
- **A copy is recorded delivered at hand-off.** The ingress receipt of a
  remote occupant's copy means the router's queue took it (the handoff
  contract). The executed evidence that a retried copy is not sent twice,
  and that the recovery of unfinished deliveries leaves a remote
  occupant's child alone, is verification's; neither has been run.
- **A slow room loses its joins.** Deciding one request, changing its
  seat and queueing its answer share 5 s. A join the room has not decided
  by then ends the seat opened for it and is answered with error presence
  (`internal-server-error`, type `wait`); any other request is answered
  as its kind is (an IQ or a message that carries something with
  `internal-server-error`, type `wait`) unless the host had begun a
  durable effect of it: a message whose commit the room began is not
  answered, since the room may have accepted it and the ingress authority
  recovers its effects, and an IQ that may have been applied is answered
  with `internal-server-error` saying so. A client whose message was
  accepted that late learns of it only from the room's copy. A room whose
  actor is slower than that under load turns its remote occupants' joins
  away until it recovers.
- **Joins where a replica carries requests.** A replica that carries
  requests to the room's holder cannot tell whether the sender of presence
  holds a seat there, so presence that does not carry the `muc` element is
  taken for a change of presence and counts against both of its account's
  shares; once those are spent it is dropped, where a join with the
  element would be answered or held. A client that enters a room without
  the element can see its join go unanswered on such a deployment.
- **The presence share is narrow.** One available presence a second on
  average, 64 at once, for each account across every room: a client that
  enters every room it may at once fits, a client that changes its
  availability often across many rooms loses some of those changes, and
  its occupants then show an earlier availability.
- **A join can go unanswered.** A join beyond its account's share of
  presence is answered only while the account's own allowance of answers
  lasts (six at once, one every ten seconds); after that it is dropped,
  so that one account's joins never hold the stream its domain's other
  accounts use. A client that sends joins faster than that waits for its
  own join timeout and retries.
- **One account can fill its domain's share of the queues.** An
  account's allowances bound how fast it sends, not how long what it sent
  waits. Its stanzas for two or more rooms whose actors are slow wait in
  its domain's share of a worker's queue, or of the placement stage (one
  room fills at most half of it); the next stanza of any account of the
  domain then holds the stream until a place frees, and the stream is
  closed with `resource-constraint` if none frees in time.
- **An account's allowances are bounded by its domain's one at a time.**
  Each of an account's allowances is at most its domain's, but their sum
  is not bounded by it: with the defaults one account can spend
  at most 21 of its domain's 50 stanzas a second and 144 of its 200 at
  once. A configuration whose per-account rates add up to more than the
  domain's lets one account spend its domain's rate, and the domain's
  stream then waits for it as it waits for all its accounts together.
- **`service-unavailable` does not end a seat.** Only the conditions
  XEP-0045 lists for ghost users (§17.1.2) do. A server answers with
  `service-unavailable` a `groupchat` message for a full JID with no such
  resource (RFC 6121 §8.5.3.2.1), but also a message from a JID its
  account blocked (XEP-0191 §3.3): an occupant that blocked another
  occupant would be removed by the room for every message that occupant
  sends. A server that answers the room's traffic for a resource that is
  gone with `service-unavailable` alone leaves a ghost seated until its
  stream is lost or the occupant's server sends its leave.
- **A leave that cannot be placed is late, or lost.** A leave or an
  error that ends a seat, for a room whose holder cannot be asked, is
  kept and placed again for up to 120 s from when it was first kept;
  meanwhile the holder still seats the occupant and sends it the room's
  traffic. One that finds no place to be kept (1,024 on a replica, 64 for
  one remote domain, 16 for one remote account) or is not placed in that
  time is dropped, and the occupant stays seated until its stream is lost
  or its seat ends otherwise. Keeping one for each room and sender means
  only the latest of several is placed. A later presence of the sender
  for the room forgets it only once placed from the replica that keeps
  it. A join that reaches another replica of the same deployment
  meanwhile does not forget the leave kept here, which can then end that
  join's occupancy once placed; the occupant enters again.
- **Leaves beyond the allowance on a replica that carries requests.** The
  first leave for a seat is taken beyond its account's allowance only on
  the replica that holds the seat, which is the replica that holds its
  room. A stream that lands on another replica, which carries the room's
  requests to the holder, holds no seat there, so a leave beyond the
  allowance it reads is dropped; the occupant stays seated until a later
  leave is taken, its stream is lost or its seat ends otherwise.
- **A stale seat row refuses an affiliation.** The admin command that sets
  a room affiliation reads the account's seats from the shared table. A row
  whose seat ended but whose removal failed refuses an owner or admin
  affiliation for its account until its replica's next pass removes it
  (rows younger than 30 s are kept) or, for a replica that stopped, until
  its rows are removed as a stopped replica's.
- **A taken-over occupant the room did not seat again is not told.** A
  seat taken over from a replica that stopped is told that it ended only
  once the room removed the occupancy recorded for it. Where the room
  seated nothing (its grants could not be read) or seated the occupant
  under another generation, the occupant is not told, and learns that it
  is not joined at its next request or self-ping.
- **A seat ends rather than lag.** An occupant whose domain's queue is
  full, or whose stream cannot be established within the dial's budget, is
  removed with status 333 and joins again. A peer that is slow for longer
  than the queue covers loses its occupants.
- **Evidence a seat was admitted under is compared, not re-verified.** A
  seat records strength, trust profile and snapshot generation. A later
  stanza is compared on strength and profile. Decisions that name the
  occupant without its request read the recorded evidence and evaluate it
  under the snapshot the replica runs now, so a home that stopped being
  trusted or admitted is refused although its seat predates the change.
- **A snapshot changes only at restart.** A replica applies a changed
  admission policy when it restarts with it, which ends its seats. Until
  every replica has restarted, one that still runs the earlier snapshot
  decides by it (ADR-0020 D1).
- **Seats of a replica that stopped without shutting down** stay in the
  table, counted against the bounds, until its liveness has lapsed for ten
  minutes or a join for the same room and occupant replaces them after
  30 s.
- **Payloads outside the allowlist are dropped.** A remote occupant's
  presence carries its availability and capabilities, and its messages the
  payloads listed above; what a third-party client sends beyond that
  (avatars in presence, mentions, references, hints, file-sharing and
  other extensions) does not reach the room. Interoperability testing
  (W10) decides which of those to review next. A correction or retraction
  from a client that names the original by an identifier other than the
  one the room expects (the message's own id for a correction, the room's
  `stanza-id` for a retraction or a reaction) is refused as a session's
  is.
- **MAM is answered but not advertised** to remote askers. XEP-0313 §7
  expects the feature in `disco#info`; like ping on the server domain this
  waits for gate G21.
- **Paging is answered but not advertised.** The MUC service's and a
  Space's `disco#items` answer XEP-0059 paging for remote askers, and a
  listing longer than one answer carries the `<set/>` XEP-0045 §6.3 asks
  for, but `http://jabber.org/protocol/rsm` is not among the features told
  to remote askers until gate G21 shows it.
- **A room's Space is not told to remote askers.** XEP-0503 has a room
  advertise its parent Space in `disco#info` (the `urn:xmpp:spaces:0`
  feature and form, and `muc#roomconfig_pubsub`). A room's `disco#info` for
  remote askers carries no extension form and no feature beyond the MUC
  ones, so a remote client learns a room's Space only from the Space's
  side; this waits for gate G21 like the other features.

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
  been idle for 60 s, or for as long as a replica seats an occupant of
  its remote domain ([how many remote domains a replica
  seats](#inbound-muc-w6a)); a local response that completes nothing still
  occupies its pair's queue until then, although it is never dialled for;
  and one unresponsive remote domain can hold at most 1,024 of a replica's
  4,096 pending requests for their lifetime. Account traffic (gate G18)
  shares the same capacity: a local account's messages and presence for a
  remote JID, and what the server sends on its behalf, open their pair's
  queue within the 64 and take places on it beside room traffic and
  replies (one account at most 32 of a pair's 64,
  [account traffic](s2s-account-traffic-2047.md)), and the errors the
  router itself returns for remote senders' account stanzas are replies
  within the 32. A
  domain whose stream cannot be established holds its pair's queue and
  the places on it until they are answered. The open-admission evidence
  must therefore include account traffic together with room traffic,
  Space notifications and replies: how many pairs and places the accounts
  of this deployment and of admitted domains take, and that room traffic
  and replies still find a queue while they do.
- **Spaces: a subscription's evidence ages.** A subscription records what
  the stream that made it proved, and keeps that evidence for as long as it
  stands. A home that later authenticates only by dialback, or under
  another trust profile, is still notified on the recorded evidence; where
  host policy requires `pkix`, that is what decides the subscriber's
  notifications. Peer admission and the trusted-home list are evaluated at
  each notification, so removing a domain from either takes effect at once.
  Subscribing again records the evidence of the new request.
- **Spaces: what one request can read.** The bounds limit how many
  requests are taken, not what each reads. `disco#items` of the service
  reads the node, the asker's affiliation and the title for each of up to
  256 federated Spaces, so one request costs up to about 770 reads, and a
  remote domain's allowance of 50 requests a second can ask for that many
  listings. A notification reads one affiliation for each of up to 256
  remote subscribers, on the Spaces host's notification task rather than in
  the publisher's request; a replica's publications wait for that task in
  one queue of 1,024, so a burst of publications to Spaces with many remote
  subscribers delays their notifications, and beyond the queue drops them.
  Neither read is cached. The `disco#items` of a Space's node reads up to
  256 items and, for each room, whether it is open to other servers and,
  for a managed room, its grants, as `<items/>` does.
- **MUC fallback: a session's listing that left a room out looks
  complete to the first-party client.** A live instant room whose actor
  does not answer within one read (1.2 s) is left out of every session's
  listing of the MUC service on its replica until it answers again, and a
  session's listing left short by a bound (10,000 catalog rows, 1,000 live
  rooms) or by a managed room whose grants cannot be read is answered the
  same way. Each such listing tells no count, but the first-party client
  reads no `<set/>` and takes it as complete, so it shows none of the
  rooms left out. Only a managed room not decided in time fails a
  session's listing, with `internal-server-error` (type `wait`).
- **MUC fallback: what one listing reads.** The MUC service's `disco#items`
  for a remote asker reads, at most once at a time on a replica and reused
  for 2 s, up to 4,096 catalog rows in one query and the actor of each live
  instant room (at most 1,000, normally none to a handful); every listing
  then reads the grants of each managed room among the rows. A replica
  builds at most four such listings at once, at most two for one remote
  domain, so one domain's allowance of 50 stanzas a second to the MUC
  service cannot take every place. A remote listing can show a room's
  catalog row or configuration as it was up to 2 s before.
- **Spaces: a notification dials.** A notification for a subscriber whose
  domain has no established stream makes this server dial it, within the
  connection manager's bounds and backoff. A remote account that subscribes
  and whose server then stops answering costs one attempt for each
  notified publication until it unsubscribes, its domain is no longer
  admitted or thirty days pass without it subscribing again.
- **Spaces: the allowance of notified publications is per replica and in
  memory.** Each Spaces host keeps it and takes from it as a publication is
  handed over, so a deployment of several replicas notifies up to the
  allowance on each, and a restart refills it. It is taken whether or not
  the Space has remote subscribers, so `waddle.s2s.spaces.events.limited`
  also counts busy Spaces that notify nobody at other servers, and up to
  4,096 Spaces spending their allowance at once keep a further Space's
  publications from being notified until one has refilled.
- **Spaces: the sweep cursor is per replica.** Each replica starts its
  sweeps of the whole store after the last domain it read itself, so two
  replicas may read the same domains in turn; each replica still reaches
  every domain in turn, in as many of its own sweeps as there are 4,096s
  of domains.
- **Spaces: what a remote owner can do.** A trusted home's owner of a Space
  holds every power a session's owner holds through PubSub, deleting the
  Space included; the deletion takes the Space's rooms out of it and
  destroys none
  ([what a remote owner of a Space can do](#what-a-remote-owner-of-a-space-can-do)).
  Trusting a home is the operator's decision to give its accounts that
  power over the Spaces they are made owners of; the recovery path is the
  admin command `spaces:set-role`.
- **Spaces: a private Space's existence under storage errors.** A read that
  fails after the `federated` setting was read answers
  `internal-server-error`, which a missing node never does. It is reached
  only for a name whose setting is on.
- **Metrics.** The router counts dropped unusable presence, dropped room
  traffic and abandoned room deliveries, and for hosted rooms the requests
  that waited, were given up or timed out at the host, were not taken for
  an account's spent share, asked for the service's room list beyond the
  lists built at once or beyond their domain's share of them, were carried
  to another replica or could not
  be, whose holder did not answer in time, the stanzas refused at the
  effect boundary, the chat states and markers dropped, the presence for
  an earlier occupancy dropped, the joins refused for want of a queue, the
  seats ended by a lost stream, an undeliverable stanza, a room that
  stopped being federated, a room that no longer seated the occupant or a
  take-over from a stopped replica, the rows reclaimed, and the messages
  refused for an account's archive allowance, all with no attribute; its
  other decisions are logged with codes only, never with an address.

## Follow-ups

- Spaces, done ([Spaces](#spaces-w7a)): the Spaces service is reached for
  the operations reviewed for a remote principal and listed in
  server-domain `disco#items` where it is.
- Spaces, an enrolled scope's Space: serving it needs a PubSub admission
  identity for the scope and a decision on what viewing a scope's Space
  means ([host-grant design](host-grant-authority-2047.md#not-decided-here));
  until then it is served to nobody, over S2S as to sessions.
- Spaces, placement by a remote principal: publishing a room into a Space
  is decided by the Space's `owner` relationship, which no writer issues to
  a remote account. A trusted home's administration of a community that
  includes placing rooms needs that relationship issued, under D8, by a
  writer of its own.
- Spaces, the `authorize` access model and its pending subscriptions, which
  Movim's Spaces use
  ([survey](../../../docs/planning/xep0503-survey-2047.md)).
- Spaces, ending a subscription on a delivery error from the subscriber's
  server (XEP-0060 §12.3), and removing at once the row of an account that
  lost its membership of a private Space; until then such a row receives
  nothing and lapses after thirty days.
- Spaces, the cost of one request: page or cache the service's listing.
- MUC fallback, the first-party client: read the `<set/>` of the MUC
  service's listing, page through a listing that does not fit in one
  answer, and take one that tells no count as incomplete
  (`chat/src/lib/xmpp/discovery.ts` reads only the items).
- Spaces, a federated setting for single peers: the setting opens a Space
  to every admitted domain.
- Spaces, the web client: offer the `federated` field in a Space's
  settings.
- Spaces, the further features XEP-0503 has a Spaces service advertise:
  with capability advertisement (W11).
- Inbound MUC, several replicas: route a domain pair's egress through one
  replica if verification with two replicas shows the alternation between
  their streams to one peer.
- D8 issuance in the Spaces relations, done: no handler, command or repair
  path names `channel.manager`, `channel.moderator`, `space.moderator` or
  `role.manager`, so the permission actor decides their issuance for every
  writer, and the Space affiliation writers decide theirs
  ([Spaces](#spaces-w7a)). The channel and Space `owner` and `admin`
  relations keep the checks of their own writers; a check of those at the
  actor as well would be a second line and is not built.
- Inbound MUC, further payloads: mentions by remote occupants, with the
  channel-wide mention of a managed room decided on the sender's stream
  evidence (it is decided without one today); references, file sharing and
  presence avatars once reviewed.
- Inbound MUC, private messages between occupants, with W6c's account
  traffic rules, and mediated invitations to remote accounts.
- Inbound MUC, a per-account share of the request rate, and an archive
  allowance that survives a restart or a claim move.
- Inbound MUC, federation per peer: the `federated` setting opens a room to
  every admitted domain; a room that names the domains it is open to needs
  a setting of its own.
- Inbound MUC, the web client: offer the `federated` field in the room
  settings.
- Inbound MUC, an unmanaged room's occupants at a domain the snapshot no
  longer admits: remove them when the replica starts with the new
  snapshot, as the revocation sweep does for a managed room.
- Local rooms, authorship of a retraction: a session's retraction is
  compared by occupant address where the archived message records no
  author; rows written before the room recorded one keep that check.
- Outbound MUC, blocking list: done with W6c's check, which applies to
  every message and presence a client addresses to a remote JID.
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
- Outbound MUC, IQ requests from rooms: done once the account side is
  attached; a remote room the resource occupies or is joining may send it
  IQ requests, its self-ping reflection included.
- Account traffic (W6c), done for the handlers it reviewed (see
  [the account-traffic design](s2s-account-traffic-2047.md)): both D7
  refusals are replaced once the account side is attached, inbound
  requests to local resources are registered for correlation, and the
  router refuses a message carrying a DM pin marker for a remote peer.
  Remote senders' direct messages are accepted through the ingress
  authority as the recipient's half of a local direct message, with the
  recipient's archive, inbox, push and carbons (G18-14); archiving local
  accounts' messages to remote contacts, with S2S egress as transport
  custody, remains.
- Remote principals at protected effects, done for the Spaces service: its
  host decides D1 with `PrincipalPolicy::witness` at each request and
  passes the principal's evidence to the host-grant decision of every room
  a Space lists, as the room host does.
- Capability advertisement (W11): any further feature in server-domain
  `disco#info` for remote askers.
- Stanzas the interpreter routes to a remote JID other than IQs, such as a
  MUC mediated invitation to a remote account, are still dropped silently
  by local resource selection; they need the same remote route.
- Router metrics with closed attributes, such as refusals by reason.
- Account traffic (W6c), done: with S2S enabled, a client's message or
  presence to a remote JID on its own blocking list is answered with
  `not-acceptable` and `<blocked/>` and not offered.
