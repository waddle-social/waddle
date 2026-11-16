# Issue 2047: S2S connection manager, admission configuration and resource model

## Status and scope

Decisions of the connection-manager slice (W4c) of
[the delivery plan](../../../docs/planning/federation-2047.md) for gate G7 of
[ADR-0020](../adrs/0020-independent-community-federation.md): the admission,
dialback and trusted-home configuration, its matching rules and how it
reaches replicas; global caps on peers, sockets and handshakes with eviction
and backoff; per-peer limits; deadlines; the shutdown join; and the
pre-authentication and post-authentication decoding limits. It also records
the owner's decisions of 2026-11-14 that followed the G4 transport review,
where they concern the connection manager, and the gate G7 decisions on
per-peer budgets, headroom and eviction, source addresses, traffic, backoff,
one stream per pair, header spellings and trusted homes on the public
profile; the codec and standards-profile parts are recorded in
[the handshake design](s2s-handshake-2047.md#decisions-after-the-g4-review).

Nothing here is executed evidence. The evidence is
`crates/waddle-server/tests/s2s_connection_manager.rs` (configuration,
admission, two in-process deployments on loopback, limits and shutdown) and,
for the codec, `crates/waddle-xmpp/tests/s2s_pre_authentication.rs` and
`crates/waddle-xmpp/tests/s2s_stream_addresses.rs`, compiled and run in the
verification phase (steps 2 and 6), and the two-process and failure steps 6
and 8.

The implementation is `crates/waddle-server/src/s2s/config.rs` and
`config/`, `crates/waddle-server/src/s2s/conn.rs` and `conn/`, and the
wiring in `server/lifecycle.rs`.

## Activation and the configuration snapshot

- S2S is off unless `WADDLE_S2S_ENABLED=true`. Only then is
  `WADDLE_S2S_CONFIG_FILE`, which must be an absolute path, read. Any other
  value of the flag, including one that is not Unicode, fails startup. While
  S2S is off no S2S file is read, no listener is bound or taken from
  `LISTEN_FDS`, and nothing is dialled.
- The file is TOML with unknown keys refused at every level. It names the
  snapshot `generation` (a UUID), the optional loopback `listen` address,
  the `local_domains` (the first is the default domain of an error-carrying
  response header), `[[credentials]]` per local domain, `[[private_ca]]`
  profiles with the remote domains that use them, `[[endpoints]]`,
  `[admission]` and `[limits]`.
- Every local domain must be the account domain, the MUC domain or the
  Spaces domain this process serves (ADR-0020 D10); anything else fails
  startup.
- Size limits: 64 KiB for the configuration file, 1 MiB for each
  certificate, key or CRL file, 16 MiB for everything one snapshot reads.
  Reads are bounded while they happen, on Tokio's blocking pool, and key
  buffers are wiped when dropped. Paths in the file must be absolute.
  Material is PEM.
- Credentials: one chain and key per local domain, the key loaded through
  the pinned ring provider and checked against the certificate. A missing,
  duplicate, unserved or mismatched credential fails the load.
- Trust: `TrustInventory` with the compiled-in Mozilla roots for every
  domain without a private-CA profile; each `[[private_ca]]` entry is loaded
  through `PrivateCaProfile::load` and configured for exactly the domains it
  lists. Operator configuration cannot substitute the public roots: a domain
  that must not trust them gets a private-CA profile instead (this settles
  the open point of the verifier design's "Substituted roots"). At load,
  `TrustInventory::summary` is logged with the allowlist, trusted homes and
  endpoint domains as the expected peers.
- **Snapshot, not reload.** The snapshot is immutable. A change of mode,
  lists, trust or limits takes effect when a replica restarts with the new
  file; a fleet change is complete when every replica runs it. Restarting
  closes every stream of the old snapshot, including those of a domain no
  longer admitted, so no separate close path is needed. A future reload
  mechanism, if one is added, closes such streams with `policy-violation`
  and is reviewed under G7 again. Files changed on disk change nothing
  until restart.
- Under the default `peer_scope = "test"`, every configured domain must be
  within `.test` and the listener and every endpoint must be loopback
  addresses; peer admission refuses any domain outside `.test` as well.
  Denylist entries are the exception: they only refuse, so they may name
  any DNS domain and an operator can pre-provision real domains.
  Admission checks the denylist before the `.test` restriction, so its
  refusal does not change when the restriction is lifted.
  `peer_scope = "public"` lifts the restriction (gate G6; see
  [the discovery design](s2s-discovery-2047.md#the-peer-scope-lifting-the-loopback-and-test-restriction)).
- The ring crypto provider is pinned for every S2S rustls configuration
  instead of the process default, so the TLS stack and the verifier's
  algorithm set cannot diverge.

## Admission (D1), dialback list (D2) and trusted homes (D8)

- Modes `closed`, `allowlist` (the default) and `open`. Every entry is
  prepared with `CanonicalDomain::prepare` and must be a DNS name
  (`DomainKind::Dns`): IP literals, numeric IPv4 forms and single-label
  names fail the load, and the same classification refuses them at stream
  open whatever the mode.
- Allowlist, dialback and trusted-home entries match exactly. A denylist
  entry covers its domain and every subdomain, label by label
  (`CanonicalDomain::is_within`), applies in every mode and overrides every
  other list. The load fails when the allowlist, the dialback list or the
  trusted-home list names a domain a denylist entry covers, when a list
  repeats an entry, when a list names a local domain, or when a denylist
  entry covers a local domain.
- A local domain is never admitted as a remote peer.
- The dialback list and the trusted-home list never admit a domain:
  `permits_dialback` and `is_trusted_home` both require the domain to be
  admitted first. Inbound dialback also needs `dialback.inbound`
  ([the dialback design](s2s-dialback-2047.md)). An entry of either list that
  the admission mode never admits (every entry in `closed` mode, one
  missing from the allowlist in `allowlist` mode) has no effect; the
  snapshot loads and a warning names the setting and the entry's position.
- **Trusted homes on the public profile.** A trusted home may authenticate
  under the public-WebPKI profile (owner decision, recorded in
  [ADR-0020 D8](../adrs/0020-independent-community-federation.md#decision-remote-authority-and-reassigned-jids-d8)
  and [the handshake design](s2s-handshake-2047.md#decisions-after-the-g4-review)).
  `VerifiedPeerStream` records the trust-profile kind, and
  `admission.require_private_ca` lists trusted homes that count as trusted
  only on a private-CA profile; each entry must be a trusted home with a
  configured private-CA profile, or the load fails. The residual risk of a
  public-profile trusted home is threefold: public chains are not checked
  for revocation (verifier design, "Revocation of public chains"), so a
  compromised key of a trusted home is usable until its certificate
  expires, at most the public leaf validity ceiling; any publicly trusted
  CA can mis-issue a certificate for the domain; and whoever gains control
  of the domain can obtain one by domain validation. Each lasts until the
  operator denylists the domain or removes it from the trusted-home list.
  Accepting that is part of the activation decision (gate G22).
- **Host policy for remote grants.** `host_policy.require_pkix` (off by
  default) makes the host-grant boundary honour a remote principal's
  grants only on a `pkix` stream, member-level grants included; privileged
  grants and room moderation need a trusted home's `pkix` stream whatever
  it says. The admission lists, the trusted-home list, `require_private_ca`
  and this switch are read at each host-grant decision from the snapshot
  the replica runs ([the host-grant design](host-grant-authority-2047.md#remote-principals-at-the-effect-boundary-g17)).
- Admission is checked at stream open (the pre-TLS header on an inbound
  stream, before dialling on an outbound one) and again before SASL
  EXTERNAL succeeds. Every refusal of the pair before TLS is
  `policy-violation`, whichever rule refused it. A probe can still tell an
  admitted domain (offered STARTTLS) from a refused one; that disclosure of
  list membership is accepted as low risk.

## Streams

- **Inbound** is STARTTLS only, on the `s2s` listener: a cold start binds
  `listen`; an inherited start takes the `LISTEN_FDS` descriptor named
  `s2s` when `listen` is configured, and an inherited `s2s` descriptor
  without it is left over and fails startup, as any unexpected descriptor
  does. The launcher bound the inherited descriptor, so startup also fails
  unless it is bound to a loopback address equal to `listen` and to the
  same port. The one exception is a `listen` port of zero, which only the
  native fixture configures: its first process binds an ephemeral port and
  its successors inherit it, so any port is accepted then, never another
  address. A nonzero `listen` port never matches a different port. There
  is no inbound direct TLS. The pre-TLS header must
  carry `from` (`bad-format` otherwise); the admitted pair fixes the
  reference identity and trust profile before the handshake.
- **Outbound** dials the endpoints the `EndpointResolver` returns: the
  configured endpoints of the domain, or, when `[discovery]` is enabled
  and none is configured, DNS discovery, with STARTTLS or XEP-0368 direct
  TLS as each endpoint says
  ([the discovery design](s2s-discovery-2047.md)). Endpoints are tried in
  order until one accepts the TCP connection, a DNS-derived address
  checked against the blocked address classes immediately before; any
  failure after that, including a received `see-other-host`, ends the
  attempt. A redirect is never followed.
- Both directions authenticate with certificate verification and SASL
  EXTERNAL, or, where `[dialback]` enables it, XEP-0220 dialback over TLS
  ([the dialback design](s2s-dialback-2047.md)). `PeerVerifier::established`
  on the completed connection is the only certificate evidence used; the
  certificate is checked again for currency before EXTERNAL succeeds on an
  inbound stream and at establishment on an outbound one. A clock that
  cannot tell the time denies authorization.
- **Registration.** A stream joins the registry, where it can replace a
  pair's stream or evict another, only when the post-SASL stream header
  completes negotiation, in both directions; a peer that stops after SASL
  displaces nothing. Before SASL EXTERNAL succeeds, the receiving role asks
  the registry, without changing it, whether the stream could be
  registered, and refuses one that could not (`conflict` or
  `resource-constraint`) before SASL succeeds. A dialler's `connect`
  returns when its own side is established, which can be just before the
  receiving side registers the stream; a refusal in that interval, which
  only a concurrent change of the registry can cause, closes the new
  stream at once.
- **`VerifiedPeerStream`** is constructed only by the connection manager
  and carries the connection id, direction, local and remote canonical
  domains (hence the verified directional pair), authentication strength
  (`pkix` or `dialback`), TLS version and cipher suite, configuration
  generation and, where the peer presented a verified certificate, its
  trust-profile kind, matched identifier kind, digest and evidence expiry;
  an inbound dialback stream without a certificate has none of the four.
- **Stanzas.** An established inbound stream hands each stanza that passes
  the codec's address checks to a `StanzaSink`. The codec reads a stanza's
  `to` and `from` with the identity profile's parser, not JID preparation,
  after mapping ASCII uppercase in the domainpart to lowercase as stream
  headers do, and hands the stanza on with both rewritten in canonical form
  (a trailing dot removed, the domain in A-labels and lowercase). A
  syntactically valid address in another spelling the profile refuses is
  handed to the sink as malformed, which may answer it with a
  `jid-malformed` stanza error and keep the stream; a sink that refuses it
  closes the stream with `improper-addressing`, as does an address that is
  not valid at all
  ([handshake design](s2s-handshake-2047.md#decisions-after-the-g4-review)).
  `RefuseAllStanzas` refuses every stanza and closes the stream with
  `policy-violation`; with S2S enabled the S2S router is the sink
  ([routing design](s2s-routing-2047.md)).
- **Outbound stanzas.** An established outbound stream writes the stanzas
  `S2sHandle::transmit` hands it, one at a time through a channel of
  capacity one, and reports whether each was written to the TLS session
  within the write deadline, was never taken (returned unwritten because
  the stream was replaced or closing), or has an unknown outcome. Stanza
  addresses are written with U-label domains, as stream headers are.
- **Duplicate pairs.** A new fully authenticated stream for a directional
  pair replaces the existing one, which closes with `conflict` (owner
  decision). To prevent two peers making a pair flap, a stream younger than
  `replacement_interval` (30 seconds by default) is kept and the new stream
  is refused with `conflict` instead. A stream never replaces one of
  greater authentication strength: while the pair has a `pkix` stream, a
  `dialback` stream for it is refused with `conflict` whatever the
  existing stream's age, and the check runs before any verification
  connection is opened for it. A `pkix` stream may replace a `dialback`
  one under the replacement interval like any other
  ([dialback design](s2s-dialback-2047.md#authentication-strength)).
- **One stream per pair is a restriction.** RFC 6120 allows more than one
  stream for a pair, and a pooled or clustered peer opens several, for
  example ejabberd with `max_s2s_connections` above one. Such a peer's
  streams replace each other: each new stream displaces the pair's stream
  once that is older than the replacement interval, so the pair's stream
  changes about once per `replacement_interval`, and a stream refused with
  `conflict` in between is retried by the peer. The harness slice (W10)
  tests clustered ejabberd with `max_s2s_connections` above one.

## Resource model

Configuration may set any value from one up to its ceiling, never beyond.
Rates and most caps default to their ceilings, so they can only be lowered;
the unauthenticated sockets per source address (2, ceiling 4), the
negotiating streams per remote domain (3, ceiling 4) and the router's pair
queues (64, ceiling 256) default below them, as do the idle timeout,
connection age, eviction idle time, replacement interval and the two
backoff durations, and can be raised to them. The input
rates (`input_per_second`, `input_burst`, `input_per_socket_per_second`,
`input_per_socket_burst`) also have a floor of 32 KiB/s, at which a stanza
at the 512 KiB ceiling arrives in 16 seconds, within the 30-second bound on
one element. The load also fails when limits do not fit together:
established streams plus unauthenticated sockets above the socket cap, a
peer cap not below the inbound socket cap, or a global input rate or burst
below the per-socket one. The values are bounds, not measured capacity.

| Resource | Value | On exhaustion |
| --- | --- | --- |
| Open sockets, all stages | 32 (16 inbound, 16 outbound) | Inbound: only a socket every other accept limit admits can find the inbound cap full. Then the established inbound stream without traffic the longest is evicted if it has had none for the eviction idle time (30 s), and the new socket takes its place under the same lock; otherwise the socket is closed at accept, no byte read. A stream outside the reserve is evicted first; in `open` mode a new socket, whose peer is unknown at accept, never evicts a reserved peer's stream. Outbound: the same with outbound streams, but only once the attempt's own TCP connection is open (G6), and an attempt to a reserved peer may evict a reserved peer's stream when no other can be evicted; otherwise the attempt fails, no backoff. Dialback verification connections are outside these counts and never evict. |
| Unauthenticated inbound sockets | 8 globally, 2 per source address (ceiling 4) | Closed at accept. |
| Tracked source addresses | 256, idle entries expire after 60 s | A new address is closed at accept while full. |
| Accept rate | 8/s burst 8 globally; 2/s burst 4 per address | Closed at accept. Every per-address limit is checked before a global token is spent, so a source over its own limits cannot drain the allowance others share. |
| TLS handshakes | 4 inbound, 2 outbound, separately | Inbound closes without XML (after `<proceed/>`, TLS cannot carry an error); outbound fails without backoff. |
| Verifier workers | One per handshake slot and as many waiters, per direction, and one per verification slot for verification connections | A waiting handshake is bounded by its stage deadline; a refused one is treated like an exhausted handshake cap. Inbound handshakes, outbound handshakes and verification handshakes can never occupy each other's workers. |
| Established streams | 24: the socket cap less the unauthenticated cap, so negotiation always has sockets. In `open` mode, 18 (all but a quarter) for peers that are neither allowlisted nor trusted homes | Evict the stream without traffic the longest if it has had none for the eviction idle time. A peer outside the reserve evicts only streams outside it, whether its general share or the whole cap is full. Otherwise refuse the new stream with `resource-constraint`. |
| Peers (remote domains with an established stream) | 12, below each direction's socket cap. In `open` mode, 9 for peers that are neither allowlisted nor trusted homes | Evict every stream of the peer whose latest traffic is oldest if none of its streams has had traffic for the eviction idle time, as for streams, and a peer outside the reserve only a peer outside it; otherwise refuse with `resource-constraint`. |
| Negotiating streams per remote domain | 3 in each direction (ceiling 4), counted separately | An inbound connection counts only once TLS has verified the remote domain, and is closed with `resource-constraint` if the domain's inbound budget is full. Outbound: the attempt fails. |
| Outbound attempts in progress | 8, one per directional pair | `ConnectError::Busy` or `InProgress`. |
| New outbound connections | 4/s burst 8, outbound attempts only; the token is taken before the endpoints are looked up | `ConnectError::RateLimited`. No backoff. A domain whose lookup found no endpoint is answered from memory for 30 s without a token (G6). |
| Dialback verification connections | 4 in progress (ceiling 4); 2/s burst 4 new ones (ceilings); slot and token taken before the Authoritative Server is looked up; outside the socket and handshake caps above, on their own verifier workers | The inbound key is answered `type='error'` with `resource-constraint`. Ordinary connections are never refused or evicted for a verification ([dialback design](s2s-dialback-2047.md#wire-profile)). |
| Outbound backoff | Failing remote domains only; 1 s doubling to 300 s; 256 entries | A domain without an entry is never refused. An entry is kept until its wait has passed: when the table is full, a new failure replaces the entry updated longest ago among those whose wait has passed, and is not recorded when every entry is still waiting (G6). |
| Input rate | 64 KiB/s burst 128 KiB per socket; 512 KiB/s burst 1 MiB globally; each at least 32 KiB/s | Reads pause (backpressure); the connection is not closed for its rate. Deadlines still bound a throttled negotiation; the incomplete-element deadline adds back the time reads waited for the rates (see [Deadlines](#deadlines)). |
| Output | rustls buffer limit 64 KiB per connection; each batch written within the 1 s write deadline | Close. |
| Router pair queues (`max_pair_queues`) | 64 (ceiling 256): the directional domain pairs the S2S router keeps a queue of outbound stanzas for. Half may be opened by what remote senders ask for | The router's, not the connection manager's: a stanza for a pair without a queue is refused, a reply to a remote sender is dropped and a remote occupant's join is refused with `resource-constraint` ([routing](s2s-routing-2047.md#egress), [inbound MUC](s2s-routing-2047.md#inbound-muc-w6a)). |

**Per-peer budgets.** A remote domain's budget is spent only by
connections that have proven it: an inbound connection counts against the
global and per-address unauthenticated caps until TLS verifies the domain
its header claimed, and against that domain's inbound budget only from
then on. Outbound attempts use a separate budget. A source that claims to
be an admitted peer therefore cannot block the real peer in either
direction; it can only use its own share of the unauthenticated caps.

**Source addresses.** An IPv4 source is counted by its address, also when
it arrives as an IPv4-mapped IPv6 address, and an IPv6 source by its /64
prefix. Per-address limits are an availability control, not an identity: a
NAT can put many peers behind one address, and a source with more address
space than a /64 can spread across prefixes. One address can hold at most
two of the four inbound handshakes, but two addresses can hold all four,
and four addresses every unauthenticated socket, delaying every other
peer's new connections until their stage deadlines close them. Both are
accepted; G6 kept the per-address limits for the public peer scope
([discovery design](s2s-discovery-2047.md#connection-manager-follow-ups-decided-here-g6)),
and activation (G20) decides them again.

**Traffic and eviction.** Traffic is a first-level element or stream header
received, or anything sent; whitespace keepalives are not traffic. They
keep a stream open, because the idle timeout counts anything received or
sent, but they do not protect it from eviction, which uses the time since
the stream's last traffic. A peer that keeps its streams busy is never
displaced, and an attacker who opens streams from many domains in `open`
mode can displace only streams that carry nothing. The reserve is a quarter
of each of the stream and peer caps, rounded down, so a cap below four
reserves nothing. What the code guarantees for the reserve's allowlisted
and trusted-home peers in `open` mode:

- A peer outside the reserve never evicts a reserved peer's stream: at
  the stream and peer caps it evicts only streams outside the reserve,
  whether its general share or the whole cap is full, and a new inbound
  socket, whose peer is unknown at accept, evicts only a stream outside the
  reserve.
- Peers outside the reserve hold at most the general share of each cap,
  so once they fill it, the remaining quarter is left to reserved peers.
- The socket caps hold no reserve. The general share of streams (18) and
  the unauthenticated sockets (8) together exceed the inbound socket cap
  (16), so busy streams of peers outside the reserve and unauthenticated
  sockets can fill every inbound socket and keep a reserved peer from
  connecting inbound until one of them ends or goes quiet; the reserved
  peer's own outbound streams are unaffected. G6 recorded this (NB1) and
  left it open: `allowlist`, the default mode, is unaffected, and it is
  decided with the switch of the default to `open` and at G20, either by
  resizing the defaults so the general share and the unauthenticated cap
  fit the inbound socket cap or by reserving inbound sockets.
- A certificate with a wildcard name, or with many names, authenticates
  one remote domain per name it covers, and each is a separate peer. One
  such certificate can therefore fill the general share of the peer and
  stream caps by itself in `open` mode. Accepted; G20 and the
  peer-identity slice (W12) decide whether peers are grouped by
  certificate or by registrable domain for these caps.

In `allowlist` mode every admitted peer is on the allowlist and the whole
of each cap is theirs, and a new socket may evict any quiet stream.

**Backoff.** Backoff is recorded only for outbound failures attributable to
the peer or the path to it: certificate, TLS and protocol failures, peer
stream errors (such as `host-unknown` or `see-other-host`), peer closes,
timeouts and I/O failures. Local capacity refusals, shutdown and a local
`conflict`, which another stream for the same pair causes, record nothing.
A success forgets the domain's entry, and an entry whose wait passed more
than the maximum backoff ago is forgotten when the next failure is
recorded, so a later failure starts again from the initial wait. The table
bounds the memory backoff uses; it never decides which domains can be
dialled. How many destinations local users can make this server dial at
once is bounded by the cap on attempts in progress. Inbound connections
never feed the backoff table. An outbound request finds the pair's
established stream and takes the pair's dial slot under one lock, and an
attempt registers its stream before it gives up the slot, so two requests
for one pair never dial twice.

For the time local users' requests can reach arbitrary domains, G6
bounds both: failures to many domains never flush a waiting backoff
entry, and a global dial rate bounds new outbound connections. A DNS
lookup failure and a domain whose every address is blocked back off like
a failed connection; a domain without endpoints is remembered for a
short while instead. See
[the discovery design](s2s-discovery-2047.md#connection-manager-follow-ups-decided-here-g6).

## Deadlines

| Deadline | Value | Renewed by input? |
| --- | --- | --- |
| TCP connect, per endpoint | 5 s | — |
| Each negotiation stage: initial header, TLS handshake, each expected restart | 5 s | No |
| SASL phase, from the post-TLS header | 5 s | No, nor by replaced or aborted exchanges |
| Dialback exchange: verifying an inbound key, or awaiting the result of an outbound one | 15 s (`dialback_timeout_ms`), within the whole negotiation | No |
| Whole negotiation, from accept or connect | 20 s | No |
| Unfinished first-level element on an established stream | 5 s of time the server is willing to read; time reads wait for the input rates is added back, up to 30 s for the element in all | No; completing an element restarts it for the next one |
| Established-stream idle | 300 s (ceiling 3,600 s) | Yes, by anything received or sent, whitespace keepalives included |
| Established-stream maximum age | 3,600 s (ceiling 24 h), and never beyond the certificate evidence's expiry | No |
| Write of one batch, terminal error and close | 1 s | — |
| Drain at shutdown | 5 s, capped by the process's remaining shutdown budget | — |

A negotiation timeout closes with `connection-timeout`, expiry and maximum
age with `reset`.

The unfinished-element deadline charges the peer only for time the server
was willing to read. The transport records how long its reads have waited
for the per-socket and global input rates, including a wait cut short by
another event, and the deadline is the element's start plus 5 s plus the
time its reads waited since then, never later than 30 s after the start. A
peer that stops sending for 5 s of readable time is closed with
`connection-timeout`, while a stanza at the 512 KiB ceiling read at the
default 64 KiB/s per-socket rate, about eight seconds after a drained burst,
arrives. When many streams share an exhausted global rate a ceiling stanza
can take longer than 30 s and is then closed; verification measures how
close a busy global rate comes to that bound.

## Shutdown join

The service's shutdown token is a child of the process stop token, so S2S
starts draining when SIGTERM or SIGQUIT arrives: the listener stops, every
stream receives `system-shutdown`, and new outbound attempts are refused. A
TCP connect or TLS handshake in progress ends at once, since neither can
carry `system-shutdown`. After the HTTP and clustering drains,
`S2sService::shutdown` stops new tasks and waits up to the drain time
within the remaining shutdown budget for every connection task. Every task
is spawned under the same lock that shutdown takes to stop new tasks, so no
task can start after the join has begun. When the drain time passes, the
remaining connections are aborted: a write, handshake or close in progress
ends and the socket is dropped, and the join waits at most the write
deadline more. The report says whether the drain and the join completed. A
verifier worker running a handshake job when its connection is dropped
finishes that job; the job holds no socket.

## Isolation from local traffic

S2S tasks run on the shared runtime but hold no lock, permit, registry or
channel shared with C2S or any local service, and certificate verification
runs on dedicated worker threads. A stalled or hostile peer can exhaust only
S2S capacity. That four occupied workers and full S2S caps leave WebSocket
clients responsive is connection-level evidence for verification steps 6
and 8.

## Decoding limits (G7)

- **Before SASL succeeds**, the decoder runs in its pre-authentication
  mode: a first-level element outside the stream, TLS, SASL and dialback
  namespaces is refused as soon as its head closes, before its tree is
  built; every first-level element, stream-namespace elements included, is
  held to the 8 KiB control-element ceiling from its first byte; and a
  connection whose first bytes are not XML (a TLS ClientHello on the
  STARTTLS port) closes without any XML. The 8 KiB header ceiling applies to
  every header.
- **After SASL succeeds**, `STREAM_CODEC_LIMITS` applies: 512 KiB per
  stanza, 4,096 elements, 384 KiB of attributes and 1.5 MiB of decoded tree
  strings per stanza, depth 32, 32 attributes per element, a 768 KiB input
  buffer and 8 KiB tokens. They are sized for a `disco#items` result of
  about 2,000 rooms and a MUC affiliation list of about 2,000 members, each
  item one element of two or three attributes under about 150 bytes.
- **Memory model**, by the codec's own accounting: an unauthenticated
  connection retains at most the header ceiling, one 8 KiB element, one
  16 KiB read and the 8 KiB token buffer, about 40 KiB. An authenticated
  connection's worst case is about 9 MiB (the input buffer, up to twice the
  tree-string ceiling in capacity, and about 4.9 MiB of node structure at
  4,096 elements with attributes), reached only by a peer that keeps one
  maximal stanza unfinished. The incomplete-element deadline ends that
  after 5 s of time the server is willing to read and at most 30 s after
  the element began, however slowly the input rates let it be read. Across
  the 24-stream cap the worst case is about 220 MiB. This is an analysis,
  not a measurement; verification measures it.
- The TLS handshake's own buffering is rustls's; the 128 KiB handshake
  allocation target of the handshake design is not shown to hold.

## Telemetry

Metrics, through the telemetry macros, with attributes from the allowlist
only (`S2sDirection`, `S2sOutcome`, `S2sResource`, `S2sTransport`,
`S2sStrength`, `S2sDialbackRole`, `S2sDialbackResult`, `S2sDiscovery`) and
never a domain, address, JID or stream id: `waddle.s2s.connections` (ended
connections by direction and outcome, including dialback refusals and
completed verification connections), `waddle.s2s.capacity.refusals` (by
direction and resource, including the dial rate and the verification
cap and rate), `waddle.s2s.established`
(by direction, transport and strength), `waddle.s2s.negotiation.duration`
(milliseconds, by direction), the `waddle.s2s.streams` gauge (by
direction), `waddle.s2s.dialback` (by role and result) and
`waddle.s2s.discovery` (by endpoint source). Logs carry the connection
id, codes and counts; certificate failures log `VerifyError::code` only.

## Follow-ups

- `_xmpps-server` direct TLS dialling, DNS discovery behind
  `EndpointResolver`, the peer scope that lifts the loopback and `.test`
  restriction and the per-address limits in the public scope are decided
  in the [discovery design](s2s-discovery-2047.md); the `dialback`
  strength, the dialback list's use and when an absent client certificate
  may reach dialback in the [dialback design](s2s-dialback-2047.md).
- The harness slice (W10): clustered ejabberd with `max_s2s_connections`
  above one, for the one-stream-per-pair restriction; a stanza whose `to`
  domain is in mixed case; and whether Prosody or ejabberd ever sends
  whitespace before a stream header, which this profile refuses.
- Measurement: the time a stanza at the 512 KiB ceiling takes when many
  streams share an exhausted global input rate, against the 30-second
  bound on one element.
- Activation (G20) and the switch of the default mode to `open` (D1): the
  socket-layer reserve (NB1, recorded in the discovery design), handshake
  monopolisation by a few source addresses and wildcard-certificate peers,
  recorded above. Backoff flush and the global dial rate are decided under
  G6; fairness of the dial rate between senders once routing dials on
  their behalf is recorded there for W5a and G20.
