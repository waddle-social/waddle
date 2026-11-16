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
  send and their bound, the deadline after which the router abandons the
  account side's work, the correlation of a remote request to a local
  resource, a domain the account side no longer admits, outbound account
  traffic, a retired account's requests, room traffic and account
  traffic, and the account's share of each pair's queue, and D7's refusal
  while no account side is attached;
- `crates/waddle-server/tests/s2s_account_traffic_ws.rs`: the RFC 6121
  account-traffic suite, a loopback S2S peer against the production `/ws`
  router and the real router: messages, presence, subscriptions in both directions, probes, IQ requests, the
  no-enumeration matrix (with a detached session, a stranger over its
  bounds and a full offline store among its states), retired JIDs and a
  retired account's open stream, D1 at the effect boundary and S2S off;
- `crates/waddle-server/tests/xep0198_s2s_account_expiry.rs`: the expiry
  of a detached XEP-0198 session holding a stranger's message, through the
  promotion chain every expiry path runs;
- `crates/waddle-server/tests/xep0160_s2s_account_offline_ws.rs`: offline
  storage of remote senders' messages and the strangers' bounds, at the
  store and at a real detached session reached by full JID, and a stored
  message whose sender's domain is no longer admitted at the flush;
- `crates/waddle-server/tests/account_jid_retirement.rs` (edited):
  retirement removes the account's rows of migrations 2010 and 2011;
- `crates/waddle-server/tests/xep0280_s2s_account_carbons_ws.rs`:
  received and sent carbons of messages exchanged with remote accounts,
  and carbon wrappers removed from remote messages;
- `crates/waddle-server/tests/xep0359_s2s_account_stanza_ids_ws.rs`:
  stanza ids in the deployment's name removed from remote messages before
  delivery and before storage, and a remote sender's own id kept;
- `crates/waddle-server/tests/xep0191_s2s_account_blocking_ws.rs`:
  blocking in both directions, and the recorded subscription requests of
  blocked senders;
- `crates/waddle-server/tests/xep0060_s2s_account_events_ws.rs`: event
  notifications from remote PubSub services and personal eventing
  services, errors carrying an event and notifications naming several
  nodes;
- `crates/waddle-server/tests/xep0199_s2s_account_ping_ws.rs`,
  `xep0115_s2s_account_caps_ws.rs` and `xep0313_s2s_account_archive_ws.rs`:
  pings between clients, capabilities in probe answers and their queries,
  and the recipient's archive, archive result wrappers removed from remote
  messages;
- `crates/waddle-server/tests/xep0410_s2s_self_ping.rs` (edited): a
  remote room's self-ping reflection reaching the occupant once an account
  side is attached;
- `crates/waddle-server/tests/s2s_reachability.rs` (edited): the account
  rows of the reachability matrix.

A case that asserts that nothing arrived ends with a barrier on the same
path as the stanza under test (a stanza on the sender's own queue, which
the account side answers whatever the account's state, then a ping on the
pair, then a ping of the client's own server and a marker the server
delivers to the client through the path the account side delivers by, its
user actor and its connection's outbound queue, or the client's own next
stanza on the pair) and inspects every stanza received since. The ping of
the client's own server alone is no barrier for deliveries: the connection
reads and answers it before it writes a delivery still waiting on its
queue.

The implementation is the router's account path in
`crates/waddle-server/src/s2s/route/` (`accounts.rs`, `router/accounts.rs`
and the hooks in `router.rs`, `room_delivery.rs` and `reach.rs`), the
stores in `crates/waddle-server/src/db/s2s_accounts.rs` with migrations
2010 and 2011, the account side in
`crates/waddle-server/src/server/routes/websocket/s2s_accounts.rs` and
`handlers/presence/remote.rs`, the outbound changes in
`handlers/unserved_route.rs`, `handlers/iq/destination_route.rs`, the
presence broadcast, block and session-end paths, the roster set and
removal, the startup roster prune, and the guard against answering a
remote sender in the offline paths (`sm_promotion/pending.rs`,
`routes/interpret/offline_delivery.rs`).

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
| Account bare or full JID | Message `chat`, `normal` | Account side, then the recipient pass of the selected resource (blocking, XEP-0359 id, XEP-0313 archive, inbox, XEP-0280 received carbons); a detached resource's session holds it raw and runs that pass when it resumes; with no resource, the account's offline store | Reviewed |
| Account bare or full JID | Message `headline` without an event | Account side, then the recipient pass | Reviewed |
| Account bare or full JID | Message carrying a XEP-0060 event | Account side, gated before anything else (an `error` included) on a subscription the account made to every node it names | Reviewed |
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

| Stanza from a non-contact | Nonexistent | Retired | Exists, offline | Exists, online | Blocks the sender | Detached session | Stranger over bounds | Offline store full |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Message `chat`, `normal`, `headline`, to the bare or a full JID | Nothing | Nothing | Nothing (stored, a `headline` dropped) | Nothing (delivered) | Nothing | Nothing (held within the bounds, a `headline` dropped) | Nothing (dropped) | Nothing (dropped) |
| Message `groupchat` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` |
| Message `error`, to the bare or a full JID | Nothing | Nothing | Nothing | Nothing | Nothing | Nothing | Nothing | Nothing |
| Presence of any type, to the bare or a full JID | Nothing | Nothing | Nothing | Nothing | Nothing | Nothing | Nothing | Nothing |
| IQ request, bare JID | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` |
| IQ request, full JID | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` | `service-unavailable` |

"Detached session" is an account whose one resource is a detached XEP-0198
session awaiting resumption; "stranger over bounds" an offline account
whose bounds for the sender are spent; "offline store full" an offline
account whose store has reached its quota.

A contact here is a JID in the account's roster with a subscription
`from` or `both`: an entity the account has authorised to see its
presence. RFC 6121 §8.1 lets the server answer such a sender with an
error where that would reveal existence or presence, because the contact
knows both already. For contacts the server therefore answers probes and
routes IQ requests to resources. A message is answered for nobody: a
contact's message that no resource takes is stored offline like a
stranger's, so its sender gets nothing back whether the account is online
or offline.

Two things a remote entity can still tell are recorded as consequences of
the protocols, not of this server:

- **A blocked contact can tell it is blocked.** A contact of an offline
  account receives `unavailable` for a probe; a contact the account blocks
  receives nothing (XEP-0191 §3.3 forbids answering it), and the
  difference shows. The same holds for local contacts.
- **Receipts and markers are the client's.** XEP-0184 receipts and
  XEP-0333 markers are sent by the recipient's client, and whether it
  answers a stranger's request for one, which reveals delivery, is the
  client's decision (XEP-0184 §8). The server neither generates nor
  suppresses them for remote senders.

Existence is checked first on every path, against the account tables and
the ledger of retired JIDs (`auth/retirement.rs`), before any roster row,
blocking list or resource is read. Retirement does not end an account's
open streams or remove every row keyed by its JID (host-grant design,
"Non-reuse of retired account JIDs"); a retired JID is nonexistent here
even while such rows or a resource remain. Retirement does remove, in its
transaction, the rows of migrations 2010 and 2011 for the account: its
recorded subscription requests, its remote PubSub subscription records
and the offline admissions of strangers' messages.

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
  message is stored in the account's offline store (XEP-0160), from a
  contact and a stranger alike, and nothing answers the sender, so that no
  sender learns whether the account is online. The classifier of local
  direct messages decides what is stored (XEP-0334 hints, chat states
  alone are not), and the account's next session receives it like any
  offline message, after its blocking list is applied again and only
  while its sender's domain is still admitted (D1, below). A stranger's
  messages are admitted within bounds per day: 32 from one sender, 128
  from one sender domain (the domain of the sender's JID, an IP literal as
  written) and 512 from all strangers together per account
  (`s2s_remote_offline_admissions`, migration 2011); a contact is held
  only to the store's own quota. A message is classified before it is
  admitted, so that one the store would not keep never uses a stranger's
  share. A message beyond a bound or the quota, or one the store refuses,
  is dropped and nothing answers it. The
  stored message is a row of its own (`Transient`): it is not written to
  the account's archive or inbox and raises no push notification, because
  those are written only through the ingress authority's commit, which has
  no producer for a remote sender in this slice (follow-ups).
- **Session expiry.** A detached stream-management session that holds a
  remote sender's message when its resume window closes hands it to the
  alternate resources and then the offline store as for any sender
  (XEP-0198 §5), but never answers its sender: beyond the store's quota
  the message is dropped, not bounced and not retained for a bounce. A
  sender is taken as remote when its `from` is neither the server domain
  nor below it; a MUC or Spaces service an operator moved outside the
  server domain is then treated the same way, its messages dropped rather
  than retained beyond the quota. A stranger's `chat` or `normal` message
  is handed to a detached session, whether the session's resource was
  selected for the bare JID or named by the full JID, only once admitted
  within the bounds above, as if it were stored at once, and one beyond
  them, or one the store would not keep (a chat state alone), is dropped.
  Nothing the store would never keep, a `headline` other than an event
  notification the account asked for, an `error` message or directed
  presence, reaches a detached full JID from a non-contact. A
  detached session's queue holds 1,000 stanzas for every sender together;
  the strangers' bound of 512 per account and day keeps them from filling
  it alone, and no bound per sender within one queue is kept. A message a
  live resource took and had not acknowledged when its stream was lost
  counts against the store's quota only, since the session's replay window
  already bounds it.
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
- **Payloads.** Before delivery or storage the server removes from a
  remote message the payloads only this server may originate for a local
  account: the Waddle inbox elements (`urn:xmpp:inbox:1` and the Waddle
  inbox namespace), the DM pin marker (`urn:waddle:pin:0`), the XEP-0280
  `<sent/>` and `<received/>` wrappers of carbon copies and the XEP-0313
  `<result/>` wrapper of archive results. A carbon or an archive result
  names the account's own traffic; one from a remote sender is a forgery
  a client could take for its own (XEP-0280 §11). Every XEP-0359
  `<stanza-id/>` this deployment could have assigned is removed too: one
  whose `by` names an address at a served domain (the recipient account,
  whose archive a client takes it for, among them) or nothing readable,
  since an entity that assigns stanza ids deletes one whose `by` it would
  set (XEP-0359 §3). It is removed here, before delivery and before
  storage, rather than left to the recipient pass, which replaces only an
  id in the name it stamps itself. One a remote entity assigned in its own
  name is left, as a routing entity leaves it (§3).

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

The read of an item, the decision and the write run under the account's
lock on the replica, inbound and outbound alike, and the client's roster
set of a remote contact takes the same lock before its own read, so that
two stanzas for one account, or a stanza and the client's own change,
never interleave their read-modify-write there and an inbound stanza
cannot restore an item the client just changed or removed. The lock is
taken before the roster's per-user mutation lock wherever both are
held. An inbound
stanza applies nothing once the router has abandoned its work (see
"Queues"). A reply or a delivery that announces a change (`subscribed`,
current or `unavailable` presence, a roster push) follows only once the
change is stored; when the store fails nothing is announced.

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
  approves or denies it, unless its sender's domain is no longer admitted
  or the account blocks its sender; when the blocking list cannot be read,
  none is delivered. Blocking a sender (XEP-0191) forgets its recorded
  request.
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
  nothing is routed. When the record of requests cannot be read, neither
  is made: the client receives a retryable `internal-server-error`.
- **Outbound `unsubscribe`** (§3.3.2) is routed, and the item loses `to`
  and any pending request.
- **Outbound `unsubscribed`** (§3.2.2): for an item with `from` or `both`,
  the contact receives `unavailable` from each available resource, then
  the `unsubscribed`, and the item loses `from`; to deny a recorded
  request, the record is removed and the `unsubscribed` routed; to cancel
  a pre-approval, the mark is removed and nothing is routed; otherwise
  nothing is routed. The account withdraws or denies on its own
  authority: the record is removed and the item loses `from` whether or
  not the route takes the stanza, and whether or not it is refused before
  the route, as to an address the account blocks (XEP-0191 §3.3) or at a
  domain no longer admitted. A refusal is still answered to the client.

A request the account side records is bounded: 256 per account and 16 per
account from one sender domain, the domain of the sender's JID as
admitted. A peer that sends from many subdomains has a share for each,
and the account's bound of 256 bounds them all together; keying by a
parent domain instead would put unrelated registrants under a public
suffix of several labels (`co.uk`) into one share, where one of them could
exhaust it for the rest. A request beyond either is dropped unanswered,
as for a nonexistent account. A recorded stanza is at most 16 KiB of serialized
XML; a larger one is dropped the same way. A request is kept 30 days: an
older one is neither delivered nor counted, and the account's next
recorded request deletes it, so the requests of a domain no longer
admitted leave the bounds by then. The count and the write are one
transaction under a lock per account: SQLite's write lock, or on
PostgreSQL a transaction-scoped advisory lock keyed by the account, so
that concurrent requests on any replica never exceed a bound. The
offline admissions of strangers' messages are counted the same way.

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
- **Full JID.** The router first asks the account side whether the
  requester's domain is still admitted (D1); one that is not gets nothing,
  not even a refusal, and nothing is read for it. Otherwise a request
  reaches the resource only when the account shares presence with the
  requester: the account's item for the requester's bare JID has `from`
  or `both` (§8.5.3.1), or the requester is a remote room the resource
  occupies or is joining (its self-ping reflection, XEP-0410 §3.2, and its
  occupants' requests). Directed
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

- the account subscribed to every node the message's events name, of that
  service: a bound resource of the account sent the service an IQ `set`
  with a `<subscribe/>` for each node, and the WebSocket layer recorded it
  in `s2s_remote_pubsub_subscriptions` (migration 2010) once the router
  took the request; an `<unsubscribe/>` the router takes removes the
  record. An event child that names no node, or a message naming more
  than 8 nodes, never passes this way;
- the sender is a remote account whose presence the account is subscribed
  to (`to` or `both`): its personal eventing service notifies the
  account's resources under XEP-0163's implicit subscription, which the
  account's presence subscription and its clients' `+notify` features
  establish.

A Space hosted on another server (XEP-0503) is used through this gate and
nothing else: joining it is the `<subscribe/>` that writes the record, and
its notifications are the events the record admits. The record admits
notifications only; it is no grant in any room the Space lists
([Spaces](s2s-routing-2047.md#spaces-w7a);
`crates/waddle-server/tests/xep0503_s2s_outbound_spaces_ws.rs`).

Anything else carrying an event is dropped unanswered, whatever the
account's state. The gate runs before anything else: a message of type
`error` carrying an event is no notification the account asked for, and is
dropped too. The record is written when the request leaves, not when
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
requests. The router then asks the account side whether the account may
send: a retired account's stream stays open until it ends, and nothing it
sends, and nothing the server sends on its behalf, leaves; a failure to
tell refuses too. The router asks this of every stanza a local account
sends to a remote JID once an account side is attached, not only of
account traffic: IQ requests and responses and room traffic are refused
for a retired account as well, before a request is reserved or a room's
occupancy written. The presence leaving a remote room for a resource that
ended is the router's own and still leaves, so that the room does not keep
an occupant that is gone. Each account stanza takes its place on the
pair's bounded queue, of which one account holds at most 32 places at
once on that pair (its own stanzas and those the server sends on its
behalf), so that one account's burst never fills a pair's queue for the
others; a stanza beyond the account's share is refused as a full queue
refuses it. The share is per pair, so that a domain whose stream cannot
be established, which holds the account's stanzas until they are
answered, never holds back the account's traffic to other domains. A message that
cannot be sent is answered with `remote-server-not-found` or
`remote-server-timeout`, and presence that cannot be sent is dropped.

The server sends on the account's behalf:

- every broadcast presence of a resource to each remote contact whose item
  has `from` or `both` (§4.2.2, §4.4.2), with the blocking check both
  ways that local contacts get;
- on the first available presence of each session, a probe from the
  account's bare JID to each remote contact whose item has `to` or `both`
  and that the account does not block (§4.2.2, §4.3.1), sent off the
  connection's task after one read of the roster and one of the blocking
  list; when the list cannot be read, no probe is sent;
- `unavailable` from a resource that ends for good to the same contacts as
  its broadcast, except contacts the account blocks (§4.5.2);
- `unavailable` to a remote contact the account blocks, and its current
  presence to one it unblocks, as for local contacts (XEP-0191 §3.3).

A message to a remote JID is not archived in the sender's archive or
projected into the sender's inbox (follow-ups). Once the router takes it,
an eligible one is copied to the sender's other Carbons-enabled resources
as a `<sent/>` carbon (XEP-0280 §7), the rule outbound MUC already uses.

## Rosters

Remote contacts stay in rosters whether S2S is enabled or not; while it
is off they are unreachable:

- The roster set accepts a contact whose domain this deployment does not
  serve when S2S is enabled (`AppState::s2s_federation`); with S2S off it
  still answers `item-not-found` for a new one, as before.
- Removing a remote contact (§2.5.2) removes a recorded request from it,
  and sends it `unsubscribe` if the item had `to` or a pending request and
  `unsubscribed` if it had `from`; nothing is sent to a contact the
  account blocks (XEP-0191 §3.3), or while S2S is off.
- The startup roster prune keeps every item whose contact is at a domain
  this deployment does not serve, with S2S enabled or not. Presence the
  server would send such a contact while S2S is off is refused by the
  route and dropped; a message to it is answered `remote-server-not-found`.

## D1, bounds and amplification

- **Effect boundary (D1).** The account side decides the sender's
  admission with `PrincipalPolicy::witness`, from the `RemotePrincipal`'s
  stream evidence and the S2S snapshot the replica runs
  (`AppState::grant_principals`), immediately before it applies anything,
  before the router reads anything for an IQ request, and again before it
  delivers a recorded subscription request at login. A domain no longer
  admitted, a denylisted parent domain included, gets nothing applied and
  nothing answered, whether the stanza arrived just now or waited on a
  queue, an IQ request included. Before the offline store is flushed to a
  session (at login, at a terminal redrive to another resource and in the
  recovery pass), the stored messages of senders at domains the
  deployment does not serve are deleted when the replica's snapshot no
  longer admits their domain, so a domain removed from the allowlist or
  denylisted after its messages were stored gets none of them delivered.
  A row is deleted only while no flush has claimed it (one statement
  checks and deletes): a flush that claimed it after the purge read the
  store is offering or has pushed it, and a delete then would lose a
  message the session may already hold. Such a row is delivered by that
  flush.
  With S2S off this is not checked: no remote traffic arrives then, the
  messages stored while it was on were admitted when they arrived, and a
  deployment without federation pays no extra read of the store at each
  login.
- **Queues.** Account traffic shares the room delivery queues and their
  workers (routing design, outbound MUC): keyed by the sender's bare JID,
  so one sender's stanzas are decided in order, with the same bounds per
  sender (64), per worker (256) and per remote domain (64 of a worker's
  256), and the same back-pressure on the stream that carried a stanza
  that does not fit. A worker waits 10 s for one stanza's account work and
  then abandons it and moves on to the sender's next stanza. The work runs
  in a task of its own, so that a write in progress is not cut short, but
  checks before every effect (a roster or store write, a delivery), under
  the account's lock where it holds one, that it has not been abandoned,
  and applies nothing more once it has: an earlier stanza's effects never
  land after a later one's from the same sender. Answers of abandoned work
  are dropped; a change it stored before the deadline stays, unannounced
  to the sender.
- **Correlated requests.** 256 inbound requests pending from one remote
  domain and 1,024 on one replica, each for at most 60 s; a request
  beyond either is answered `resource-constraint`. A request whose
  response leaves from another replica, where the requested resource's
  session is, holds its place on the replica that registered it until its
  lifetime ends, so for such requests the bounds are a rate: 256 per
  domain and 1,024 per replica per 60 s.
- **Replies.** One stanza produces at most 16 replies to its sender, all
  on the pair's reply share: the probe answers of up to 16 resources, or
  one error, one `subscribed`, or one `unavailable` per resource. Nothing
  received produces traffic to any other remote JID.
- **Stored state** per account: 256 recorded subscription requests (16
  per sender domain), each kept 30 days; 1,024 PubSub subscription
  records; and the offline admissions of strangers' messages of the last
  day (at most 512). The offline store's own quota (1,000 rows per account
  by default) bounds the messages stored.

## Behaviour with S2S off

Without a router none of this runs: the remote route stays
`FederationDisabled`, no account side is attached and nothing reaches an
account from a stream. Behaviour visible while S2S is disabled:

- a message or presence to a remote JID is answered
  `remote-server-not-found` before any roster or blocking check: the
  blocking check and the roster half of an outbound subscription run only
  with S2S enabled; an `unsubscribed` to a remote JID removes a recorded
  request from it first, which only matters for a request recorded while
  S2S was on;
- the roster set refuses a new remote contact with `item-not-found`, as
  before;
- **changed (G24):** the startup roster prune no longer removes contacts
  at domains the deployment does not serve. Before, a deployment with S2S
  off removed every such item at each start; now it keeps them, dormant,
  and they are reachable again once S2S is enabled. Junk items at foreign
  domains that the prune used to remove stay until their owner removes
  them.

Migration 2010 creates `s2s_pending_subscriptions` and
`s2s_remote_pubsub_subscriptions`, and migration 2011
`s2s_remote_offline_admissions`, on both engines; the owner phase grants
the runtime `SELECT, INSERT, UPDATE, DELETE` on all three. They stay
empty while S2S is off. They belong to no restricted same-pool runtime
store. This is the slice's rollout part (G24).

## G18 decision record

| # | Decision | Alternatives considered |
| --- | --- | --- |
| G18-1 | The router stays the only entry point; the account side is one typed entry taking a `RemotePrincipal`, attached at startup; without it D7's interim refusal holds in both directions. | Dispatching S2S stanzas to the client handlers (rejected: they assume a local session and write both rosters). |
| G18-2 | No enumeration: for a non-contact, every class has one remote-visible result whatever the account's state (table above). Existence, retirement included, is checked first. | RFC 6121 §8.5.1 error for nonexistent accounts and offline storage for existing ones (rejected: distinguishes them). |
| G18-3 | IQ requests to the bare JID are answered `service-unavailable`; no namespace is answered on an account's behalf for remote requesters. | Enabling vCard, PEP and account discovery (deferred: each needs its own access review). |
| G18-4 | IQ requests to a full JID reach the resource for a requester with `from` or `both`, or a remote room the resource occupies, registered as `inbound` correlation rows. | Directed-presence admission (not tracked). |
| G18-5 | Messages go to one most available resource; with none, the message is stored offline, from a contact and a stranger alike, and nothing ever answers a remote sender's message (owner decision, 2026-11-16). Strangers are held to bounds per sender, sender domain and account, at a detached session as at the store; beyond a bound or the quota a message is dropped silently. The minimal producer writes a `Transient` offline row; archive, inbox and push wait for the remote ingress producer. | `service-unavailable` to contacts and dropping strangers' messages (replaced: a contact could tell an offline account from an online one); erroring to everybody (rejected: presence leak). |
| G18-6 | Broadcast presence is accepted only from contacts the account is subscribed to; probes are answered only for contacts with `from` or `both`, and otherwise ignored. | §8.5.2.1.2 delivery from anyone; `unsubscribed` to every unauthorised prober. |
| G18-7 | Subscriptions: the account's own roster half only; inbound requests recorded durably, bounded, delivered at each new session; pre-approval supported. | The client path's in-memory pending queue (rejected: per replica, lost on restart, unbounded across requesters). |
| G18-8 | Event notifications only for a node the account subscribed to, recorded when the request leaves, or from the PEP service of a contact the account is subscribed to. | Recording on the service's result (rejected: the `<subscription/>` element is optional and pending subscriptions would be missed). |
| G18-9 | D1 is decided at the account side, from the stream's evidence and the snapshot the replica runs, before any effect and again before a recorded request is delivered. | Deciding at enqueue only. |
| G18-10 | Remote contacts stay in rosters with S2S off, dormant; the roster set may still refuse a new one while it is off (owner decision, 2026-11-16). | Pruning them at every start with S2S off (replaced: a deployment that turns S2S off for a while lost its users' remote contacts). |
| G18-11 | No path answers a remote sender's message: the account side, the expiry of a detached session and the ingress quota bounce all drop instead of bouncing a sender at a domain the deployment does not serve. | Bouncing on quota as for local senders (rejected: tells the sender the account is offline). |
| G18-12 | The router abandons account work at its deadline and the account side then applies nothing more; roster read-modify-write runs under a per-account lock; a change is announced only once stored. | Letting abandoned work run to its end (replaced: its effects could land after the sender's next stanza's). |
| G18-13 | A retired account's open stream sends nothing to remote JIDs, account traffic, IQ requests and responses and room traffic alike, refused at the router with a fail-closed check; retirement removes the account's rows of migrations 2010 and 2011. | Ending the stream at retirement (out of scope: retirement does not end streams); refusing account traffic only (replaced: requests and room traffic still left). |

## Restrictions and risks

- **Offline messages from remote senders are stored outside the archive.**
  They reach the account's next session but are not in its archive or
  inbox and raise no push notification; a stranger's message beyond its
  bounds, or any beyond the store's quota, is lost without notice.
- **Strangers' bounds are a rate.** They count admissions of the last
  day, not the messages still stored; the store's quota bounds those.
- **A detached session's queue has no bound per sender.** Strangers
  together are held to 512 admissions per account and day, below the
  queue's 1,000 stanzas, but a contact's stanzas are bounded by that
  queue only, and a queue that fills drops its oldest stanzas, a
  stranger's held message among them.
- **Stored messages after S2S is turned off.** The admission of their
  senders' domain is checked again before a flush only while S2S is
  enabled; messages stored while it was on are delivered after it is
  turned off.
- **Outbound messages are not archived** in the sender's archive or
  projected into the sender's inbox; the sender's other devices see them
  only as carbons while online.
- **The account lock is per replica.** Inbound and outbound subscription
  handling for one account and the client's own roster set, on two
  replicas, can still interleave their read-modify-write of one item; the
  last write wins. The bounded stores are serialised across
  replicas by their transaction lock.
- **Remote sender detection at session expiry** reads the `from` of the
  stored stanza; a service moved outside the server domain is treated as
  remote there.
- **One stream per pair under load.** A pair's queue holds 64 stanzas, of
  which one account holds at most 32 at once. A session's first presence
  sends a contact with `both` a probe and a broadcast, so a roster with
  more than about 16 such contacts on one domain, or 32 with `from` only,
  can find the account's share full at login while the pair's stream
  drains it; the excess is dropped silently, and those contacts see the
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
  to the 10 s bound per stanza; work abandoned at that bound applies
  nothing more but its task lives until its current read or write
  returns, so a store that hangs accumulates such tasks.
- **Every stanza an account sends to a remote JID reads the account's
  existence.** Every message, presence and IQ for a remote JID, room
  traffic and the server's own stanzas on the account's behalf included,
  costs the two reads of the existence check while an account side is
  attached.

## Follow-ups

- A remote ingress producer for direct messages, so that messages from
  remote senders are stored offline through the ingress commit, with the
  recipient's archive, inbox and push notification, and outbound messages
  archived and projected, with S2S egress recorded as transport custody
  only. Inbound MUC's `IngressPrincipal::Remote` does not serve it as it
  stands, so the account side's `Transient` producer (`store_offline` in
  `s2s_accounts.rs`) stays: the principal's acceptance
  (`ingress::principal::assert_admission`) admits only a message to a
  room's bare JID, decided on the room's host-grant witness, and its one
  plan (`plan_remote_occupant_groupchat`) is a room's acceptance, in which
  the room is the only authority that stamps the message. A direct
  message needs a plan of its own in which the recipient account is that
  authority and no archive of the sender's is written (it is kept by the
  sender's server), an acceptance arm for an account target that decides
  the D1 admission and the strangers' bounds inside the commit, and the
  ingress receipt for a remote sender, none of which exists yet. When it
  does, the producer keeps the account side's rules: the strangers'
  bounds, the silent drop, and no bounce to a remote sender
  (`sm_promotion::is_remote_sender`, which already guards the ingress
  quota bounce in `offline_delivery.rs`). A message a live resource takes
  already passes the recipient pass, which archives it and projects it
  into the recipient's inbox; only a stored one misses those.
- Remote access to vCard-temp, PEP items, account discovery and last
  activity, each with its own review, and PEP notifications from local
  accounts to remote contacts subscribed to them.
- Directed presence tracking for remote entities.
- Advertising any of this, in the server domain's `disco#info` for remote
  askers or elsewhere, stays with capability advertisement (W11).

## Decisions

- **Offline storage for remote senders (owner, 2026-11-16).** Messages from
  remote senders to an offline account are stored, from contacts and
  strangers alike, with no difference observable to the sender and no
  bounce; strangers' messages are subject to blocking and to bounds per
  sender and per domain, beyond which they are dropped silently. An
  offline account's contact gets nothing back either (G18-5, G18-11).
- **Remote contacts with S2S off (owner, 2026-11-16).** They stay in
  rosters, dormant; the roster set may still refuse a new one (G18-10).
- **A blocked contact can tell it is blocked** from an offline account; this
  is accepted as a consequence of XEP-0191 §3.3 (see "No enumeration").
- **Receipts and markers** are a client's duty, per XEP-0184 §8 (see "No
  enumeration").
- **A retired account's still-open session** sends nothing to remote
  JIDs, refused at the router for every stanza it sends and failing closed
  (G18-13). A local room's stanzas for remote occupants leave from the
  room's address, through the seats of inbound MUC, not through that
  check: a retired account's messages never reach them, since retiring
  the account deletes its sessions with it and the room's ingress then
  refuses the session's principal, but its presence in a local room it
  still occupies, which the room handles without the ingress commit,
  reaches the room's remote occupants until its session ends.
