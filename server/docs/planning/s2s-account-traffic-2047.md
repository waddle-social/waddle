# Issue 2047: traffic between remote senders and local accounts

## Status and scope

Decisions of the account-traffic slice (W6c) of
[the delivery plan](../../../docs/planning/federation-2047.md) for gate G18
of [ADR-0020](../adrs/0020-independent-community-federation.md): which
local-account handlers a stanza from an S2S stream can reach, the RFC 6121
server rules each one follows for a remote sender, and the matching
outbound traffic of local accounts (D7). It replaces the interim refusal
that [the routing design](s2s-routing-2047.md#account-traffic-in-the-interim-d7)
records, in both directions, for the handlers reviewed here, and leaves it
in force for everything else.

The rules were read against the pinned sources in `xeps/`: RFC 6121 (from
the RFC Editor) and the XSF sources at
`4132c531487fe9450ae90b905b8df950d0d478b2`: XEP-0060 1.30.0, XEP-0115
1.6.0, XEP-0191 1.3, XEP-0199 2.0.1, XEP-0280 1.0.1 and XEP-0313 1.1.3.

Nothing here is executed evidence. The evidence is written and compiled but
not run until verification (D9):

- `crates/waddle-server/tests/s2s_account_router.rs`: the router's side,
  with a recording account side: what reaches it and in which order, the
  queue it shares with room traffic and its bounds, the replies it may
  send and their bound, the correlation of a remote request to a local
  resource, the PubSub subscription record and its gate, outbound
  account traffic, and D7's refusal while no account side is attached;
- `crates/waddle-server/tests/s2s_account_traffic_ws.rs`: the RFC 6121
  account-traffic suite, a loopback S2S peer against the production `/ws`
  router and the real router: messages, presence, subscriptions in both
  directions, probes, IQ requests, the no-enumeration matrix, retired
  JIDs, D1 at the effect boundary and S2S off;
- `crates/waddle-server/tests/xep0280_s2s_account_carbons_ws.rs`:
  received and sent carbons of messages exchanged with remote accounts;
- `crates/waddle-server/tests/xep0191_s2s_account_blocking_ws.rs`:
  blocking in both directions;
- `crates/waddle-server/tests/xep0060_s2s_account_events_ws.rs`: event
  notifications from remote PubSub services and personal eventing
  services;
- `crates/waddle-server/tests/s2s_reachability.rs` (edited): the account
  rows of the reachability matrix.

The implementation is the router's account path in
`crates/waddle-server/src/s2s/route/` (`accounts.rs`, `router/accounts.rs`
and the hooks in `router.rs`, `room_delivery.rs` and `reach.rs`), the
stores in `crates/waddle-server/src/db/s2s_accounts.rs` with migration
2010, the account side in
`crates/waddle-server/src/server/routes/websocket/s2s_accounts.rs` and
`handlers/presence/remote.rs`, and the outbound changes in
`handlers/unserved_route.rs`, `handlers/iq/destination_route.rs`, the
presence broadcast, block and session-end paths, the roster set and
removal, and the startup roster prune.

## The seam

The router stays the only entry point for stanzas from S2S streams (gate
G23). No handler written for client sessions is given a remote `from`:
those handlers stamp the sender from a session, write both rosters of a
pair, claim a user actor for the sender or ignore `to`. The account side is
one typed entry, `LocalAccounts`, that the WebSocket layer implements and
startup attaches to the router beside the local delivery path. It takes a
`RemotePrincipal`, the local address and the stanza, and returns the
stanzas, if any, that go back to the sender's domain.

Account traffic is enabled as a unit by attaching that account side. A
router without one, as in the routing and outbound MUC suites, keeps D7's
interim refusal in both directions exactly as the routing design records
it.

## Reachability inventory (G18)

Every row names the handler a stanza from a remote principal reaches and
its status. "Not enabled" means the stanza is refused as if the account did
not exist (RFC 6121 §8.5.1). Account state never changes which of these
rows applies.

| Destination | Stanza | Handler | Status |
| --- | --- | --- | --- |
| Account bare JID | IQ `get`/`set` | Router: `service-unavailable` on the account's behalf (RFC 6121 §8.5.2.1.3) | Reviewed: no namespace is answered on an account's behalf for remote requesters |
| Account bare JID | IQ `result`/`error` | Dropped | Reviewed |
| Account full JID | IQ `get`/`set` | Router, correlation table, local delivery to that resource | Reviewed: only for a requester the account shares presence with, or a remote room the resource occupies |
| Account full JID | IQ `result`/`error` | Completes the resource's own request (routing design) | Unchanged |
| Account bare or full JID | Message `chat`, `normal` | Account side, then the recipient pass of the selected resource (blocking, XEP-0359 id, XEP-0313 archive, inbox, XEP-0280 received carbons); a detached resource's session holds it raw and runs that pass when it resumes | Reviewed |
| Account bare or full JID | Message `headline` without an event | Account side, then the recipient pass | Reviewed |
| Account bare or full JID | Message carrying a XEP-0060 event | Account side, gated on a subscription the account made | Reviewed |
| Account bare or full JID | Message `groupchat` | Account side: `service-unavailable`, whatever the account | Reviewed (room traffic is decided first, by the outbound MUC rules) |
| Account bare or full JID | Message `error` | Delivered to a matching full JID; otherwise dropped | Reviewed |
| Account bare JID | Presence available or `unavailable` | Account side: every available resource, from a contact the account is subscribed to | Reviewed |
| Account full JID | Presence available or `unavailable` | Account side: that resource, if bound (directed presence) | Reviewed (room presence is decided first) |
| Account bare or full JID | Presence `subscribe`, `subscribed`, `unsubscribe`, `unsubscribed` | Account side: the account's own roster half and the pending-request store | Reviewed |
| Account bare or full JID | Presence `probe` | Account side: answered for a contact with a subscription from the account | Reviewed |
| Account bare or full JID | Presence `error` | Dropped | Reviewed |
| Account bare or full JID | Presence, or a message other than a mediated invitation or its decline, carrying a room's `muc#user` element, from a room the resource does not occupy | Dropped (stray room traffic) | Reviewed |
| vCard-temp, PEP items, account `disco#info` and `disco#items`, last activity, private storage, roster, blocking, MAM at the bare JID | IQ | — | Not enabled: each needs its own review of access for a remote requester and of what its answer discloses |

## No enumeration

The organising constraint, which G18 decides, is that a remote sender who
is not the account's contact learns nothing from the server about whether
the account exists, has been retired, is online, or blocks it. For such a
sender every row above produces the same remote-visible result in every
account state:

| Stanza from a non-contact | Nonexistent | Retired | Exists, offline | Exists, online | Blocks the sender |
| --- | --- | --- | --- | --- | --- |
| Message `chat`, `normal`, `headline` | Nothing | Nothing | Nothing (dropped) | Nothing (delivered) | Nothing |
| Message `groupchat` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` |
| Message `error` | Nothing | Nothing | Nothing | Nothing | Nothing |
| Presence of any type | Nothing | Nothing | Nothing | Nothing | Nothing |
| IQ request, bare JID | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` |
| IQ request, full JID | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` |

A contact here is a JID in the account's roster with a subscription
`from` or `both`: an entity the account has authorised to see its
presence. RFC 6121 §8.1 lets the server answer such a sender with an
error where that would reveal existence or presence, because the contact
knows both already. For contacts the server therefore answers a message
it cannot deliver with `service-unavailable`, answers probes, and routes
IQ requests to resources.

Existence is checked first on every path, against the account tables and
the ledger of retired JIDs (`auth/retirement.rs`), before any roster row,
blocking list or resource is read. Retirement does not end an account's
open streams or remove every row keyed by its JID (host-grant design,
"Non-reuse of retired account JIDs"); a retired JID is nonexistent here
even while such rows or a resource remain.

## RFC 6121 server rules, per class

### Messages (RFC 6121 §8.5.2.1.1, §8.5.2.2.1, §8.5.3)

- **Selection.** A `chat` or `normal` message to the bare JID goes to the
  one most available resource: the live available resource with the
  highest non-negative priority, ties broken by the resource's address;
  when none is available, a connected resource that has not sent presence
  yet, as local delivery to the bare JID does (RFC 6121 §8.5.2.1 speaks of
  an available or connected resource). A resource that is available with
  a negative priority never receives one (§8.5.2.1.1).
  Delivering to one resource lets its recipient pass archive the message
  once, project it into the inbox once and copy it as a `<received/>`
  carbon to the account's other Carbons-enabled resources (XEP-0280 §6),
  which delivering to several would each do again. With no live
  candidate, a detached stream-management resource that is available is
  used; its session holds the message raw until it resumes, and the
  recipient pass (archive, inbox, carbons) runs on its replay. A `headline`
  goes to every available resource with non-negative priority (§8.5.2.1.1)
  and is not archived or projected.
- **Full JID.** A message to a full JID that is bound, live or detached,
  goes to it. A `chat` message to a full JID that is not bound is handled
  as if sent to the bare JID (§8.5.3.2.1); `normal` and `headline` are
  dropped.
- **No resource.** RFC 6121 §8.5.2.2.1 asks the server to store a `chat`
  or `normal` message offline or return `service-unavailable`. The
  server's offline store is written only through the ingress authority's
  commit, which has no producer for a remote sender yet, so this slice
  does not store; a contact's message receives `service-unavailable`, and
  a non-contact's message is dropped, which is neither option of the
  SHOULD. Returning the error to everybody would tell a stranger whether
  the account is online, a presence leak §8.1 asks the server to avoid.
  This is recorded as a limitation; storing offline needs the remote
  ingress producer (follow-ups).
- **`groupchat`** to an account is answered with `service-unavailable`
  (§8.5.2.1.1 and §8.5.2.2.1 require an error for the bare JID; the full
  JID takes the same answer so that no state is distinguishable).
  Traffic from a remote room the resource occupies never reaches this
  rule: the outbound MUC rules decide it first.
- **`error`** messages are delivered to a bound full JID and otherwise
  dropped, never answered.
- **Stray room traffic.** A remote room's traffic reaches an occupant
  through the router's occupancy table before the account side sees it
  (routing design, outbound MUC). Presence carrying the room's `muc#user`
  element, and a message carrying it other than a mediated invitation or
  its decline (XEP-0045 §7.8.2), that still reaches the account side
  comes from a room the resource never joined or has left, and is
  dropped.
- **Payloads.** Before delivery the server removes from a remote message
  the payloads only this server may originate for a local account: the
  Waddle inbox elements (`urn:xmpp:inbox:1` and the Waddle inbox
  namespace) and the DM pin marker (`urn:waddle:pin:0`). XEP-0359 ids
  naming this server are removed by the recipient pass, as for any peer
  stanza.

### Presence (RFC 6121 §4, §8.5.2.1.2, §8.5.3.1)

- **Available and `unavailable` to the bare JID** reach every available
  resource only when the account's roster item for the sender's bare JID
  has a subscription `to` or `both`, the subscription under which a
  contact's server broadcasts presence (§4.2.3, §4.4.3, §4.5.3). Presence
  from anybody else is dropped. This narrows §8.5.2.1.2, which delivers
  presence to the bare JID from any sender; §8.1 leaves such delivery to
  local privacy policy, and presence from non-contacts carries status text
  nobody asked for.
- **To a full JID** it reaches that resource if bound (directed presence,
  §8.5.3.1) and is dropped otherwise (§8.5.3.2.2).
- **Probes** (§4.3.2) are answered only for a sender whose bare JID the
  account's roster holds with `from` or `both`: with the last presence of
  each available resource, from its full JID and with its stored
  extension payloads (XEP-0115 caps among them), or with `unavailable`
  from the bare JID when none is available. A probe to a full JID is
  answered for that resource only, with available presence without
  details. A probe from anybody else is dropped. §4.3.2 says the server
  SHOULD answer it with `unsubscribed`; answering only some senders would
  separate a blocked sender (XEP-0191 §3.3 forbids answering it) and an
  existing account from a nonexistent one, so the server answers none.
- **`error`** presence is dropped.

### Subscriptions (RFC 6121 §3, Appendix A)

Only the local account's half of a subscription is the server's: the
contact's roster is on its own server. The account side therefore reads
and writes the account's roster item for the contact and never creates a
roster item for an inbound request (§3.1.3, security warning).

- **Inbound `subscribe`**, to the bare or a full JID (§3.1.3 treats the
  latter as the former). If the account's item for the sender has `from`
  or `both`, the server answers `subscribed` on the account's behalf
  (§3.1.3 rule 2). If the item carries a pre-approval (§3.4), the server
  approves on the account's behalf: the item gains `from`, the
  pre-approval is cleared, the account's interested resources receive a
  roster push, the sender receives `subscribed` and then the current
  presence of each available resource (§3.1.5). Otherwise the request is
  recorded in `s2s_pending_subscriptions` (migration 2010), one per sender
  and account, replacing an earlier one (§3.1.3 rule 4), and delivered to
  every available resource. A recorded request is delivered again on the
  first available presence of every new session until the account
  approves or denies it.
- **Inbound `subscribed`** (§3.1.6) changes the item only if it has a
  pending outbound request (`ask`): `none` becomes `to`, `from` becomes
  `both`; the stanza is delivered to the interested resources, followed by
  the roster push. Without the pending request it is ignored, so a peer
  cannot write itself into a roster.
- **Inbound `unsubscribe`** (§3.3.3) removes a recorded request from the
  sender. If the item has `from` or `both`, it loses `from`, the stanza
  and the roster push reach the interested resources, and the sender
  receives `unavailable` from each available resource.
- **Inbound `unsubscribed`** (§3.2.3) changes an item with `to`, `both` or
  a pending request: it loses `to` and the request, and the stanza and the
  roster push reach the interested resources. Otherwise it is ignored.
- **Outbound `subscribe`** (§3.1.2) is stamped with the account's bare JID
  and offered to the router; once the router takes it, the item gains a
  pending request and the roster push follows. A request the route refuses
  changes nothing.
- **Outbound `subscribed`** (§3.1.5) to a contact with a recorded request
  is routed; the item gains `from`, the record is removed, the roster push
  follows and the contact receives the current presence of each available
  resource. Without a recorded request or `from`, it is a pre-approval
  (§3.4.2): the item is marked approved, the roster push follows, and
  nothing is routed.
- **Outbound `unsubscribe`** (§3.3.2) is routed, and the item loses `to`
  and any pending request.
- **Outbound `unsubscribed`** (§3.2.2): for an item with `from` or `both`,
  the contact receives `unavailable` from each available resource, then
  the `unsubscribed`, and the item loses `from`; to deny a recorded
  request, the record is removed and the `unsubscribed` routed; to cancel
  a pre-approval, the mark is removed and nothing is routed; otherwise
  nothing is routed.

A request the account side records is bounded: 256 per account and 16 per
account from one remote domain. A request beyond either is dropped
unanswered, as for a nonexistent account. A recorded stanza is at most
16 KiB of serialized XML; a larger one is dropped the same way.

### IQ (RFC 6121 §8.5.2.1.3, §8.5.3.1)

- **Bare JID.** The server answers every request on the account's behalf
  with `service-unavailable`. §8.5.2.1.3 requires that answer for any
  namespace whose reply the server does not provide on the user's behalf,
  and no namespace is enabled for remote requesters: the existing handlers
  for vCard-temp, PEP items, account discovery and last activity read the
  requester from a local session, check access models against local
  presence only, or answer without an existence check, and the blocking
  handler ignores `to` altogether. Each is a later reviewed change to this
  inventory.
- **Full JID.** A request reaches the resource only when the account
  shares presence with the requester: the account's item for the
  requester's bare JID has `from` or `both` (§8.5.3.1), or the requester
  is a remote room the resource occupies or is joining (its self-ping
  reflection, XEP-0410 §3.2, and its occupants' requests). Directed
  presence is not tracked, so it does not admit a request. A request that
  is admitted is registered in the correlation table as an `inbound` row
  before it is delivered, and only the resource's own response to the
  requester with the same id completes it and leaves (routing design,
  "Correlation"). A request to a resource that is not bound, or whose
  delivery fails, is withdrawn and answered with `service-unavailable`,
  as are requests from anybody else.
- **XEP-0115 and XEP-0199.** Capability queries (`disco#info` with a
  caps node) and pings between clients go to full JIDs, so they work
  between contacts and nowhere else. A ping to an account's bare JID is
  answered `service-unavailable`; XEP-0199 §4.4 asks clients to ping a
  full JID because the server handles a bare one.

### Event notifications (XEP-0060 §7.1, XEP-0163)

A message carrying a `pubsub#event` payload from a remote entity is
delivered, under the headline rules above, only when one of these holds:

- the account subscribed to that node of that service: a bound resource
  of the account sent the service an IQ `set` with a `<subscribe/>` for
  the node, and the WebSocket layer recorded it in
  `s2s_remote_pubsub_subscriptions` (migration 2010) once the router took
  the request; an `<unsubscribe/>` the router takes removes the record;
- the sender is a remote account whose presence the account is subscribed
  to (`to` or `both`): its personal eventing service notifies the
  account's resources under XEP-0163's implicit subscription, which the
  account's presence subscription and its clients' `+notify` features
  establish.

Anything else carrying an event is dropped unanswered, whatever the
account's state. The record is written when the request leaves, not when
the service answers: a subscription the service makes pending or refuses
lets through only events the account asked for, and a result without the
`<subscription/>` element (XEP-0060 §6.1.2 makes it optional) still
counts. An account holds at most 1,024 records; a further `<subscribe/>`
is refused with `resource-constraint`.

## Outbound account traffic

With the account side attached the router takes from a bound resource of
a local account, besides IQ requests and room traffic:

- messages of type `chat`, `normal`, `headline` and `error` to any remote
  JID that is not room traffic of the sending resource (a `groupchat`
  message still leaves only for a room the resource has joined);
- directed presence, available, `unavailable` and `error`, to any remote
  JID that is not room traffic;
- from the account's bare JID, subscription presence and probes.

A message carrying the DM pin marker is refused (`service-unavailable`):
DM pins are local only. Before offering a message or presence, the
WebSocket layer checks the sender's own blocking list and answers a match
with `not-acceptable` and `<blocked/>` (XEP-0191 §3.3), as for IQ
requests. Each account stanza takes its place on the pair's bounded
queue; a message that cannot be sent is answered with
`remote-server-not-found` or `remote-server-timeout`, and presence that
cannot be sent is dropped.

The server sends on the account's behalf:

- every broadcast presence of a resource to each remote contact whose item
  has `from` or `both` (§4.2.2, §4.4.2), with the blocking check both
  ways that local contacts get;
- on the first available presence of each session, a probe from the
  account's bare JID to each remote contact whose item has `to` or `both`
  and that the account does not block (§4.2.2, §4.3.1);
- `unavailable` from a resource that ends for good to the same contacts as
  its broadcast, except contacts the account blocks (§4.5.2);
- `unavailable` to a remote contact the account blocks, and its current
  presence to one it unblocks, as for local contacts (XEP-0191 §3.3).

A message to a remote JID is not archived in the sender's archive or
projected into the sender's inbox (follow-ups). Once the router takes it,
an eligible one is copied to the sender's other Carbons-enabled resources
as a `<sent/>` carbon (XEP-0280 §7), the rule outbound MUC already uses.

## Rosters

Remote contacts are kept in the roster only while S2S is enabled:

- The roster set accepts a contact whose domain this deployment does not
  serve when S2S is enabled (`AppState::s2s_federation`); with S2S off it
  answers `item-not-found` as before.
- Removing a remote contact (§2.5.2) sends it `unsubscribe` if the item
  had `to` or a pending request and `unsubscribed` if it had `from`, and
  removes a recorded request from it.
- The startup roster prune keeps items whose contact is at a domain this
  deployment does not serve when S2S is enabled, and removes them, as
  before, when it is off.

## D1, bounds and amplification

- **Effect boundary (D1).** The account side decides the sender's
  admission with `PrincipalPolicy::witness`, from the `RemotePrincipal`'s
  stream evidence and the S2S snapshot the replica runs
  (`AppState::grant_principals`), immediately before it applies anything,
  and again before it delivers a recorded subscription request at login.
  A domain no longer admitted, a denylisted parent domain included, gets
  nothing applied and nothing answered, whether the stanza arrived just
  now or waited on a queue.
- **Queues.** Account traffic shares the room delivery queues and their
  workers (routing design, outbound MUC): keyed by the sender's bare JID,
  so one sender's stanzas are decided in order, with the same bounds per
  sender (64), per worker (256) and per remote domain (64 of a worker's
  256), and the same back-pressure on the stream that carried a stanza
  that does not fit. One stanza's account work may take 10 s before the
  worker moves on.
- **Correlated requests.** 256 inbound requests pending from one remote
  domain and 1,024 on one replica, each for at most 60 s; a request
  beyond either is answered `resource-constraint`.
- **Replies.** One stanza produces at most 16 replies to its sender, all
  on the pair's reply share: the probe answers of up to 16 resources, or
  one error, one `subscribed`, or one `unavailable` per resource. Nothing
  received produces traffic to any other remote JID.
- **Stored state** per account: 256 recorded subscription requests (16
  per remote domain) and 1,024 PubSub subscription records.

## Behaviour with S2S off

Without a router none of this runs: the remote route stays
`FederationDisabled`, no account side is attached and nothing reaches an
account from a stream. Behaviour visible while S2S is disabled is
unchanged:

- a message or presence to a remote JID is answered
  `remote-server-not-found` before any roster or blocking check: the
  blocking check and the roster half of an outbound subscription run only
  with S2S enabled;
- the roster set and the startup prune treat remote contacts as before.

Migration 2010 creates `s2s_pending_subscriptions` and
`s2s_remote_pubsub_subscriptions` on both engines, and the owner phase
grants the runtime `SELECT, INSERT, UPDATE, DELETE` on both. Both stay
empty while S2S is off. They belong to no restricted same-pool runtime
store. This is the slice's rollout part (G24).

## G18 decision record

| # | Decision | Alternatives considered |
| --- | --- | --- |
| G18-1 | The router stays the only entry point; the account side is one typed entry taking a `RemotePrincipal`, attached at startup; without it D7's interim refusal holds in both directions. | Dispatching S2S stanzas to the client handlers (rejected: they assume a local session and write both rosters). |
| G18-2 | No enumeration: for a non-contact, every class has one remote-visible result whatever the account's state (table above). Existence, retirement included, is checked first. | RFC 6121 §8.5.1 error for nonexistent accounts and offline storage for existing ones (rejected: distinguishes them). |
| G18-3 | IQ requests to the bare JID are answered `service-unavailable`; no namespace is answered on an account's behalf for remote requesters. | Enabling vCard, PEP and account discovery (deferred: each needs its own access review). |
| G18-4 | IQ requests to a full JID reach the resource for a requester with `from` or `both`, or a remote room the resource occupies, registered as `inbound` correlation rows. | Directed-presence admission (not tracked). |
| G18-5 | Messages go to one most available resource; offline messages are not stored until a remote ingress producer exists; a contact gets `service-unavailable`, others nothing. | Storing outside the ingress commit (rejected: bypasses the effect acceptance contract); erroring to everybody (rejected: presence leak). |
| G18-6 | Broadcast presence is accepted only from contacts the account is subscribed to; probes are answered only for contacts with `from` or `both`, and otherwise ignored. | §8.5.2.1.2 delivery from anyone; `unsubscribed` to every unauthorised prober. |
| G18-7 | Subscriptions: the account's own roster half only; inbound requests recorded durably, bounded, delivered at each new session; pre-approval supported. | The client path's in-memory pending queue (rejected: per replica, lost on restart, unbounded across requesters). |
| G18-8 | Event notifications only for a node the account subscribed to, recorded when the request leaves, or from the PEP service of a contact the account is subscribed to. | Recording on the service's result (rejected: the `<subscription/>` element is optional and pending subscriptions would be missed). |
| G18-9 | D1 is decided at the account side, from the stream's evidence and the snapshot the replica runs, before any effect and again before a recorded request is delivered. | Deciding at enqueue only. |
| G18-10 | Remote contacts in rosters exist only while S2S is enabled; the startup prune removes them when it is off. | Keeping them while off (owner question below). |

## Restrictions and risks

- **Offline messages from remote senders are not stored.** A contact
  learns the message was not delivered; a stranger's message to an
  offline account is lost without notice.
- **Outbound messages are not archived** in the sender's archive or
  projected into the sender's inbox; the sender's other devices see them
  only as carbons while online.
- **One stream per pair under load.** A roster with more than 64 remote
  contacts on one domain overflows that pair's queue when presence is
  broadcast; the excess is dropped silently, and those contacts see the
  change at the next broadcast or probe.
- **Presence from non-contacts is dropped**, which narrows RFC 6121
  §8.5.2.1.2; directed presence to a full JID is still delivered.
- **Probes from unauthorised senders are not answered with
  `unsubscribed`**, so a remote server whose user thinks it holds a
  subscription the account does not grant learns it only from the
  account's own `unsubscribed`.
- **Local accounts' personal eventing notifications do not reach remote
  contacts**: the PEP fan-out delivers to local resources only.
- **Directed presence is not tracked**, so an entity the account sent
  directed presence to is neither told of the account's unavailability
  nor admitted to send IQ requests to its resources.
- **The account side runs on the four room-traffic workers.** A slow
  roster or account read slows every remote sender sharing a worker, up
  to the 10 s bound per stanza.
- **Disabling S2S removes remote contacts** at the next start.

## Follow-ups

- A remote ingress producer, shared with inbound MUC (W6a), so that
  messages from remote senders are stored offline and outbound messages
  archived and projected through the ingress commit, with S2S egress
  recorded as transport custody only.
- Remote access to vCard-temp, PEP items, account discovery and last
  activity, each with its own review, and PEP notifications from local
  accounts to remote contacts subscribed to them.
- Directed presence tracking for remote entities.
- Advertising any of this, in the server domain's `disco#info` for remote
  askers or elsewhere, stays with capability advertisement (W11).

## Owner questions

- Should a deployment that disables S2S keep its users' remote contacts
  instead of pruning them at the next start?
- Is dropping a stranger's message to an offline account acceptable until
  offline storage for remote senders exists, or should such messages be
  answered `service-unavailable` at the cost of revealing that the account
  is offline?
