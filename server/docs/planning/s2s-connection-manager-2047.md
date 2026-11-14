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
- Until the discovery slice lifts it (gate G6), every configured domain
  must be within `.test` and the listener and every endpoint must be
  loopback addresses; peer admission refuses any domain outside `.test` as
  well. Denylist entries are the exception: they only refuse, so they may
  name any DNS domain and an operator can pre-provision real domains.
  Admission checks the denylist before the `.test` restriction, so its
  refusal does not change when the restriction is lifted.
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
  admitted first. Dialback is not implemented in this release; the list is
  validated and kept for the dialback slice. An entry of either list that
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
  public-profile trusted home is that public chains are not checked for
  revocation (verifier design, "Revocation of public chains"): a
  compromised key of a trusted home is usable until its certificate
  expires, at most the public leaf validity ceiling, unless the operator
  denylists the domain. Accepting that is part of the activation decision
  (gate G22).
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
  unless it is bound to a loopback address equal to `listen`; port zero in
  `listen`, which only the native fixture uses, matches the port the first
  process bound. There is no inbound direct TLS. The pre-TLS header must
  carry `from` (`bad-format` otherwise); the admitted pair fixes the
  reference identity and trust profile before the handshake.
- **Outbound** dials only configured endpoints through the
  `EndpointResolver` trait, which the discovery slice implements, with
  STARTTLS only: XEP-0368 direct TLS is deferred to the discovery slice with
  `_xmpps-server` discovery (owner decision). Endpoints are tried in order
  until one accepts the TCP connection; any failure after that, including a
  received `see-other-host`, ends the attempt. A redirect is never followed.
- Both directions authenticate with certificate verification and SASL
  EXTERNAL. `PeerVerifier::established` on the completed connection is the
  only evidence used; the certificate is checked again for currency before
  EXTERNAL succeeds on an inbound stream and at establishment on an outbound
  one. A clock that cannot tell the time denies authorization.
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
  (`pkix`; `dialback` is reserved for the dialback slice), trust-profile
  kind, matched identifier kind, TLS version and cipher suite, leaf
  certificate digest, evidence expiry and configuration generation.
- **Stanzas.** An established inbound stream hands each stanza that passes
  the codec's address checks to a `StanzaSink`. The codec reads a stanza's
  `to` and `from` with the identity profile's parser, not JID preparation,
  and hands the stanza on with both rewritten in canonical form (a
  trailing dot removed, the domain in A-labels). The sink of this release
  refuses every stanza, and the stream closes with `policy-violation`; the
  routing slice replaces it.
- **Duplicate pairs.** A new fully authenticated stream for a directional
  pair replaces the existing one, which closes with `conflict` (owner
  decision). To prevent two peers making a pair flap, a stream younger than
  `replacement_interval` (30 seconds by default) is kept and the new stream
  is refused with `conflict` instead.
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
Caps and rates default to their ceilings, so they can only be lowered; the
idle timeout, connection age, eviction idle time, replacement interval and
the two backoff durations default below their ceilings and can be raised to
them. The values are bounds, not measured capacity.

| Resource | Value | On exhaustion |
| --- | --- | --- |
| Open sockets, all stages | 32 (16 inbound, 16 outbound) | Inbound: when the inbound cap is full, the established inbound stream without traffic the longest is evicted if it has had none for the eviction idle time (30 s), and the new socket takes its place; otherwise the socket is closed at accept, no byte read. Outbound: the same with outbound streams; otherwise the attempt fails, no backoff. |
| Unauthenticated inbound sockets | 8 globally, 4 per source address | Closed at accept. |
| Tracked source addresses | 256, idle entries expire after 60 s | A new address is closed at accept while full. |
| Accept rate | 8/s burst 8 globally; 2/s burst 4 per address | Closed at accept. Every per-address limit is checked before a global token is spent, so a source over its own limits cannot drain the allowance others share. |
| TLS handshakes | 4 inbound, 2 outbound, separately | Inbound closes without XML (after `<proceed/>`, TLS cannot carry an error); outbound fails without backoff. |
| Verifier workers | One per handshake slot and as many waiters, per direction | A waiting handshake is bounded by its stage deadline; a refused one is treated like an exhausted handshake cap. Inbound handshakes can never occupy the outbound workers. |
| Established streams | 24: the socket cap less the unauthenticated cap, so negotiation always has sockets. In `open` mode, 18 (all but a quarter) for peers that are neither allowlisted nor trusted homes | Evict the stream without traffic the longest if it has had none for the eviction idle time; a peer outside the reserve evicts only streams outside it. Otherwise refuse the new stream with `resource-constraint`. |
| Peers (remote domains with an established stream) | 12, below each direction's socket cap. In `open` mode, 9 for peers that are neither allowlisted nor trusted homes | Evict every stream of the peer whose latest traffic is oldest if none of its streams has had traffic for the eviction idle time, as for streams; otherwise refuse with `resource-constraint`. |
| Negotiating streams per remote domain | 2 in each direction, counted separately | An inbound connection counts only once TLS has verified the remote domain, and is closed with `resource-constraint` if the domain's inbound budget is full. Outbound: the attempt fails. |
| Outbound attempts in progress | 8, one per directional pair | `ConnectError::Busy` or `InProgress`. |
| Outbound backoff | Failing remote domains only; 1 s doubling to 300 s; 256 entries | A domain without an entry is never refused. When the table is full, a new failure replaces the entry updated longest ago among those whose wait has passed, or else the entry updated longest ago. |
| Input rate | 64 KiB/s burst 128 KiB per socket; 512 KiB/s burst 1 MiB globally | Reads pause (backpressure); the connection is not closed for its rate. Deadlines still bound a throttled negotiation. |
| Output | rustls buffer limit 64 KiB per connection; each batch written within the 1 s write deadline | Close. |

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
space than a /64 can spread across prefixes. While S2S is limited to
loopback endpoints and `.test` domains (before gate G6) that is accepted;
it is decided again with discovery (G6) and at activation (G20).

**Traffic and eviction.** Traffic is a first-level element or stream header
received, or anything sent; whitespace keepalives are not traffic. They
keep a stream open, because the idle timeout counts anything received or
sent, but they do not protect it from eviction, which uses the time since
the stream's last traffic. A peer that keeps its streams busy is never
displaced, and an attacker who opens streams from many domains in `open`
mode can displace only streams that carry nothing, never those of the
reserve's allowlisted and trusted-home peers once the general share is
full. The reserve is a quarter of each cap, rounded down, so a cap below
four reserves nothing. In `allowlist` mode every admitted peer is on the
allowlist and the whole cap is theirs.

**Backoff.** Backoff is recorded only for outbound failures attributable to
the peer or the path to it: certificate, TLS and protocol failures, peer
stream errors (such as `host-unknown` or `see-other-host`), peer closes,
timeouts and I/O failures. Local capacity refusals and shutdown record
nothing. A success forgets the domain's entry, and an entry whose wait
passed more than the maximum backoff ago is forgotten when the next failure
is recorded, so a later failure starts again from the initial wait. The
table bounds the memory backoff uses; it never decides which domains can be
dialled. How many destinations local users can make this server dial at
once is bounded by the cap on attempts in progress. Inbound connections
never feed the backoff table.

## Deadlines

| Deadline | Value | Renewed by input? |
| --- | --- | --- |
| TCP connect, per endpoint | 5 s | — |
| Each negotiation stage: initial header, TLS handshake, each expected restart | 5 s | No |
| SASL phase, from the post-TLS header | 5 s | No, nor by replaced or aborted exchanges |
| Whole negotiation, from accept or connect | 20 s | No |
| Unfinished first-level element on an established stream | 5 s | No; completing an element restarts it for the next one |
| Established-stream idle | 300 s (ceiling 3,600 s) | Yes, by anything received or sent, whitespace keepalives included |
| Established-stream maximum age | 3,600 s (ceiling 24 h), and never beyond the certificate evidence's expiry | No |
| Write of one batch, terminal error and close | 1 s | — |
| Drain at shutdown | 5 s, capped by the process's remaining shutdown budget | — |

A negotiation timeout closes with `connection-timeout`, expiry and maximum
age with `reset`.

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
  maximal stanza unfinished, which the 5 s incomplete-element deadline ends.
  Across the 24-stream cap the worst case is about 220 MiB. This is an
  analysis, not a measurement; verification measures it.
- The TLS handshake's own buffering is rustls's; the 128 KiB handshake
  allocation target of the handshake design is not shown to hold.

## Telemetry

Metrics, through the telemetry macros, with attributes from the allowlist
only (`S2sDirection`, `S2sOutcome`, `S2sResource`, `S2sTransport`) and never
a domain, address, JID or stream id: `waddle.s2s.connections` (ended
connections by direction and outcome), `waddle.s2s.capacity.refusals` (by
direction and resource), `waddle.s2s.established` (by direction and
transport), `waddle.s2s.negotiation.duration` (milliseconds, by direction)
and the `waddle.s2s.streams` gauge (by direction). Logs carry the connection
id, codes and counts; certificate failures log `VerifyError::code` only.

## Follow-ups

- The discovery slice: `_xmpps-server` direct TLS dialling, DNS discovery
  behind `EndpointResolver`, and lifting the loopback and `.test`
  restriction (G6), with the per-address limits decided again.
- The dialback slice: the `dialback` strength, the dialback list's use, and
  its own decision whether an absent client certificate may reach dialback
  (the receiving TLS configuration requires one today).
- Routing: replace `RefuseAllStanzas`, carry the stream's strength and
  trust-profile kind into the remote principal, and address stanzas to
  peers in U-labels as stream headers already are.
- The harness slice (W10): clustered ejabberd with `max_s2s_connections`
  above one, for the one-stream-per-pair restriction.
- Measurement: a stanza at the 512 KiB ceiling sent at the per-socket input
  rate (64 KiB/s after a 128 KiB burst) takes about six seconds, longer
  than the 5 s unfinished-element deadline. Verification measures whether
  the ceilings, the rate or the deadline need to change together.
