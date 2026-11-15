# Issue 2047: S2S discovery, addressing and the peer scope

## Status and scope

Decisions of the discovery and dialback slice (W4d) of
[the delivery plan](../../../docs/planning/federation-2047.md) for gate G6
of [ADR-0020](../adrs/0020-independent-community-federation.md): the
discovery profile of D3 (lookups, ordering, "." targets, failure versus
absence, lookups without a search list, refused domain forms), the
completed list of blocked address classes, the caps on targets and
addresses, XEP-0368 direct TLS for outbound connections, the reviewed
change that lifts the loopback and `.test` restriction, and the follow-ups
the [connection-manager design](s2s-connection-manager-2047.md) left to
G6. D3 fixes the policy; this document records how it is implemented and
the values D3 left to this slice.

Gate G6 must be reviewed before W4d merges. Discovery is off by default,
loads only with `peer_scope = "public"`, and the restriction to `.test`
domains and loopback addresses stays in force unless the configuration
lifts it.

Nothing here is executed evidence. The evidence is
`crates/waddle-server/tests/xep0368_s2s_discovery.rs`, compiled and run in
the verification phase (step 2): a stub resolver driving discovery; the
address classes and the operator's blocked prefixes; `SystemDnsLookup`
against a name server in the test process on loopback, which pins how
Hickory DNS 0.26.3 classifies answers and that only the exact absolute
name is queried; configuration; and direct TLS and dialling cases on
loopback. Discovery has no other evidence and no test-only way around
these checks: the interoperability harness (W10) and step 6 reach their
peers through configured endpoints, so they cover configured endpoints,
refused domain forms and redirects, not DNS.

The implementation is `crates/waddle-server/src/s2s/discovery.rs`,
`discovery/address.rs` and `discovery/system.rs`, the endpoint and dialling
changes in `crates/waddle-server/src/s2s/conn/`, the direct-TLS entry of
`crates/waddle-xmpp/src/s2s/negotiation/initiating.rs`, and the
`peer_scope` and `[discovery]` settings.

Sources: XEP-0368 1.1.0 at xsf/xeps
`4132c531487fe9450ae90b905b8df950d0d478b2`, RFC 6120 §3.2, RFC 2782,
RFC 7590, RFC 9525 §6.1.1 and the IANA IPv4 and IPv6 Special-Purpose
Address Registries.

## Configuration

```toml
peer_scope = "test"   # or "public"; top-level key

[discovery]
enabled = false            # true only with peer_scope = "public"
blocked_prefixes = []      # e.g. ["2001:db8:64::/96"], CIDR, at most 64

[[endpoints]]
domain = "peer.example"
address = "192.0.2.10:5269"
transport = "starttls"   # or "direct_tls"
```

- Configured endpoints (RFC 6120 §3.2.3) replace discovery for their
  domain, select a socket and a TLS entry only, never change the
  reference identity, and are the only exemption from the address checks.
- With `[discovery] enabled = true`, a domain without configured
  endpoints is discovered through DNS. The resolver is built at startup
  from the system's name-server configuration; a configuration that
  cannot be read or names no name server fails startup instead of
  silently disabling discovery.
- `enabled = true` fails to load (`DiscoveryRequiresPublicScope`) unless
  `peer_scope = "public"`. In the test scope every admitted domain is
  within `.test` and every configured endpoint is loopback, so discovery
  could only ever resolve to addresses the blocked classes refuse; a
  configuration that enables it there is a mistake, not a working
  setting.
- `blocked_prefixes` lists address blocks refused for DNS-derived
  addresses in addition to the built-in classes (below). Each entry is
  CIDR notation with its host bits clear and a prefix length from one up
  to the family's width; a malformed entry, a duplicate or more than 64
  entries fail the load.

## Discovery profile

To reach domain D (its canonical form, lowercase A-labels):

1. **Refused forms.** If the identity profile classifies D as anything but
   a DNS name (a bracketed IPv6 literal, a dotted-quad IPv4 address, a
   numeric, hexadecimal or abbreviated IPv4 form such as `2130706433`,
   `0x7f.1` or `127.1`, or a single-label name), nothing is looked up or
   dialled. Admission already refuses such domains; discovery refuses them
   again.
2. **SRV.** `_xmpps-server._tcp.D.` and `_xmpp-server._tcp.D.` are queried
   concurrently. Each answer is records, negative (NXDOMAIN, or a
   successful response without SRV records) or failed (SERVFAIL, REFUSED,
   a timeout, a malformed response or any other error). At most 64 records
   of one answer are considered. An answer with SRV records whose targets
   cannot be read as absolute names is still records, not negative: the
   service is published, so those records forbid the fallback like any
   others, and contribute no target.
3. **"." targets.** A record set containing a "." target says that service
   is not offered. Its "." records are dropped; the other set is still
   used; A/AAAA fallback is forbidden for D. D3 speaks of a set whose only
   target is "."; a "." mixed with other targets is treated the same way,
   which only ever forbids a fallback.
4. **Ordering.** Records of both sets are ordered together as one set
   (XEP-0368 §3, RFC 2782 §"Usage rules"): ascending priority; within a
   priority, records of weight zero first, then repeated weighted random
   selection among the records left. `_xmpps-server` targets are reached
   with direct TLS, `_xmpp-server` targets with STARTTLS.
5. **Targets.** A target whose name the identity profile does not classify
   as a DNS name is skipped. The first six targets that remain are
   resolved, A and AAAA concurrently; records from either lookup are used
   when the other fails, and a failure of either with no usable address
   from the other is a failed lookup, never absence. Addresses in a
   blocked class or an operator-listed block are removed, the rest
   interleaved IPv6 first and at most four kept per target, in order.
6. **Outcome.** If SRV records produced at least one address, those are
   the endpoints, in order. If no SRV records exist: a failed SRV lookup
   ends the attempt with `LookupFailed` (no fallback, retried later with
   backoff); a "." target ends it with no endpoint; and only when both
   lookups were negative are D's own A and AAAA records used on port 5269
   with STARTTLS, with the same address rules and the same four-address
   cap. When nothing usable remains the reason is, in order, a failed
   lookup of any kind, SRV or address, including a failed SRV set beside
   records whose targets have no address (`LookupFailed`), only blocked
   addresses (`Blocked`), or nothing published (`NoEndpoints`).
7. **Bounds.** One domain's discovery takes at most 10 s, every lookup
   included; each query waits 3 s for a name server and is sent twice. A
   lookup failure and an all-blocked result back the domain off like a
   failed connection. No endpoint at all, and a domain that is not a DNS
   name, are not failures and record no backoff; they are remembered for
   30 seconds instead, and a repeated request for the domain within that
   time gets the same answer without a dial token or a lookup.

Recorded deviation: RFC 6120 §3.2.1 says an initiating entity that
receives no response to its SRV query SHOULD fall back to A/AAAA records.
Waddle does not, because an attacker who can suppress DNS responses could
otherwise bypass published records, including a "." target (D3).

**No search list.** Every query is for an absolute name ending with a dot.
The system resolver keeps only the system's name servers: its default
domain and search list are dropped and the hosts file is never consulted,
so neither a search domain nor a hosts entry can complete or redirect a
name; the name servers themselves still answer as they choose, which
D3 accepts because DNS never authenticates. The `DnsLookup` trait
carries absolute names only, which the stub suite asserts, and the
loopback name-server tests assert that `SystemDnsLookup`, built from a
configuration that names a default domain and a search list, puts only
the exact names it was given to the name server. The hosts-file test can
assert only where the test machine's hosts file maps a name other than
`localhost`, which Hickory answers itself under RFC 6761 whatever the
configuration.

**DNS never authenticates.** The reference identity, the TLS server name
and the direct-TLS Server Name Indication are always D. SRV target names
are never reference identities (RFC 9525 §6.1.1), and an answer only
selects sockets: a poisoned answer leads to a connection whose certificate
fails for D. DNSSEC is not validated.

**Resolver.** `hickory-resolver` 0.26.3 is already in `Cargo.lock` through
`libp2p-dns`, with the `system-config` and `tokio` features this slice
uses enabled there, so no new package enters the lockfile. It reports a
negative answer (`NoRecordsFound` with NXDOMAIN or NOERROR) separately
from every failure, which D3's distinction between failure and absence
needs and the standard library's resolver cannot express.

## Blocked address classes

Every DNS-derived address, from an SRV target or the A/AAAA fallback, is
refused if it is in one of these classes or in a block the operator lists
in `discovery.blocked_prefixes`, and the dialler, for streams and for
dialback verification connections alike, connects only to the exact
address it checked, checking it again immediately before the TCP
connect. A changed DNS answer cannot substitute another address,
because nothing is re-resolved between the check and the connect.

| Class | Blocked |
| --- | --- |
| "This network", unspecified | `0.0.0.0/8`; all IPv6 outside `2000::/3`, which includes `::/128` |
| Loopback | `127.0.0.0/8`; `::1/128` |
| Private use, unique local | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`; `fc00::/7` |
| Link-local | `169.254.0.0/16`; `fe80::/10` (and the deprecated site-local `fec0::/10`) |
| Shared address space (CGNAT) | `100.64.0.0/10` |
| IETF protocol assignments | `192.0.0.0/24` (DS-Lite, dummy, NAT64 discovery, PCP and TURN anycast); `2001::/23` except Teredo |
| Documentation | `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`; `2001:db8::/32`, `3fff::/20` |
| Benchmarking | `198.18.0.0/15`; `2001:2::/48` (within `2001::/23`) |
| Deprecated 6to4 relay anycast | `192.88.99.0/24` |
| Multicast | `224.0.0.0/4`; `ff00::/8` |
| Reserved and broadcast | `240.0.0.0/4` including `255.255.255.255/32` |
| Discard-only, dummy, SRv6 SIDs, local-use NAT64 | `100::/64`, `100:0:0:1::/64`, `5f00::/16`, `64:ff9b:1::/48` (all outside `2000::/3`) |
| IPv4-mapped and IPv4-compatible | `::ffff:0:0/96` and `::/96`, refused outright |
| NAT64 well-known prefix | `64:ff9b::/96` when the embedded IPv4 address is blocked |
| 6to4 | `2002::/16` when the embedded IPv4 address is blocked |
| Teredo | `2001::/32` when the server address or the de-obfuscated client address is blocked |
| ISATAP interface identifiers | `…:0:5efe:a.b.c.d` and `…:200:5efe:a.b.c.d` in any prefix when the embedded IPv4 address is blocked |

Decisions completing D3's table from the IANA registries:

- Every block the registries mark as not globally reachable is blocked.
- IPv6 is checked against the global unicast space `2000::/3`: everything
  outside it is reserved, multicast, link-local, unique local, or
  otherwise no server address, and is blocked, except the NAT64
  well-known prefix, whose embedded address decides.
- Globally reachable special-purpose blocks inside a blocked block are
  blocked with it: the PCP and TURN anycast addresses in `192.0.0.0/24`,
  and the anycast, AMT, AS112, ORCHIDv2 and drone remote-ID blocks in
  `2001::/23`. No XMPP server is reached at them. AS112 and AMT blocks
  outside a blocked block (`192.31.196.0/24`, `192.52.193.0/24`,
  `192.175.48.0/24`, `2620:4f:8000::/48`) are not blocked; they are
  ordinary unicast space as far as a dialler is concerned.
- An IPv4-mapped or IPv4-compatible address in an AAAA record is refused
  whatever it embeds: an AAAA record has no reason to carry one, and the
  A record carries the IPv4 address, which is checked as such. This is
  stricter than D3's row, which blocks a mapped address only when its
  embedded address is blocked.
- Embedded IPv4 addresses are checked for NAT64, 6to4, Teredo (both
  addresses) and ISATAP. Network-specific NAT64 prefixes (RFC 6052) and
  6rd (RFC 5969) embed IPv4 addresses under operator-chosen prefixes that
  an address alone does not reveal, so the operator of a network that
  uses them lists those prefixes in `discovery.blocked_prefixes`, which
  refuses every address within them. In a DNS64 network that also blocks
  the synthesised addresses of IPv4-only peers, which are then reached
  only through configured endpoints. An operator-listed IPv4 block also
  applies to the IPv4 address embedded by NAT64, 6to4, Teredo and ISATAP.

## Direct TLS (XEP-0368)

- A direct-TLS endpoint (an `_xmpps-server` target or a configured
  `direct_tls` endpoint) is dialled with TLS from the first byte: the
  initiating machine's direct-TLS entry starts with the TLS handshake,
  with no plaintext header and no `<starttls/>`, then sends its first
  stream header over TLS.
- The Server Name Indication is the remote domain D (XEP-0368 §3: the
  JID's domain part), the `xmpp-server` ALPN protocol is offered, and the
  certificate is verified exactly as on a STARTTLS connection. A server
  that selects no ALPN protocol is accepted; rustls refuses one that
  selects another.
- A post-TLS features element that offers STARTTLS ends the stream with
  `policy-violation`, as on any stream after TLS: STARTTLS is never used
  over direct TLS.
- Metrics carry `transport="direct_tls"` on established streams.
- There is no inbound direct TLS and no direct-TLS listener (D2).

## The peer scope (lifting the loopback and `.test` restriction)

`peer_scope = "test"` is the default and restricts peers as the
connection-manager design describes: every configured domain except
denylist entries within `.test`, the listener and every configured
endpoint on a loopback address, and admission refusing every domain
outside `.test`.

`peer_scope = "public"` lifts it: local domains, allowlist, dialback,
trusted-home and private-CA entries may be any fully qualified DNS domain,
admission admits domains outside `.test` by its lists, the listener and
configured endpoints may be any address, discovery may be enabled, and an
inherited `s2s` listener must still be bound to exactly the configured
address (on any interface rather than loopback only). Nothing else
differs between the scopes: the identity profile refuses IP-address,
numeric IPv4 and single-label names in every list and at stream open; the
denylist, which may name any domain, overrides every list in both
scopes; DNS-derived addresses are checked against the blocked classes in
both scopes.

Discovery is available only in the public scope. Lifting the scope on a
deployment is part of its activation decision (G20 and G22), not of this
slice. The connection-manager design sizes the per-address limits and the
inbound socket reserve for the test scope; for the public scope the
per-address limits are kept and the inbound socket reserve (NB1) stays
open until the switch to `open` and G20, as recorded below.

## Connection-manager follow-ups decided here (G6)

- **Global dial rate.** New outbound connections to every remote domain
  together are limited to 4 per second with a burst of 8
  (`limits.dial_per_second`, `limits.dial_burst`, which can only be
  lowered), so attempts that fail quickly cannot follow one another
  without pause. An attempt that finds no token fails with
  `ConnectError::RateLimited` and records nothing in the backoff table.
  Dialback verification connections are not charged here; they have a
  budget of their own ([dialback design](s2s-dialback-2047.md#wire-profile)).
- **Dial-rate fairness, for W5a and G20.** The token is taken before the
  endpoints are looked up, so an attempt for a domain that turns out to
  have no endpoint, or not to be a DNS name, spends a token although
  nothing is dialled, and records nothing in the backoff table. Such a
  result is remembered for 30 seconds, so repeated requests for the same
  dead domain spend no further token. Distinct dead domains still cost one
  token each: once routing (W5a) calls `connect` on behalf of senders,
  one sender naming many distinct dead domains can use up the dial rate
  and starve dials for everyone else. The rate is global, not per sender;
  W5a and the activation decision (G20) decide per-sender fairness before
  routing dials on senders' behalf.
- **Backoff entries are not flushable.** An entry is kept at least until
  its wait has passed. When the table (256 entries) is full, a new
  domain's failure replaces only the entry updated longest ago among those
  whose wait has passed; when every entry is still waiting, the new
  failure is not recorded. A sender who makes this server dial many
  failing domains can therefore never shorten another domain's wait; a
  domain whose failure was not recorded is dialled again only within the
  global dial rate. Entries are forgotten when their wait passed more than
  the maximum backoff ago.
- **Outbound eviction only after the TCP connect.** An attempt takes an
  outbound socket without eviction before it connects. When the outbound
  socket cap is full, it connects first and evicts a stream without
  traffic only once its own TCP connection is open; if nothing can be
  evicted then, it drops the connection and fails with `capacity`. An
  unreachable destination never displaces a stream. While connecting, such
  a socket is outside the socket counts; the cap on attempts in progress
  (8) bounds how many exist. Dialback verification connections are
  outside the outbound socket counts altogether, bounded by their own cap
  (4), and never evict.
- **Per-address limits** (2 unauthenticated sockets per source address,
  accept rates per address, IPv6 counted per /64) are kept for the public
  scope. They remain an availability control, not an identity; a source
  with more address space can spread across prefixes, as recorded in the
  connection-manager design.
- **NB1, the socket-layer reserve in `open` mode,** is not solved by this
  slice. The general share of streams (18) and the unauthenticated sockets
  (8) together exceed the inbound socket cap (16), so busy streams of
  peers outside the reserve and unauthenticated sockets can fill every
  inbound socket and keep a reserved peer from connecting inbound until
  one of them ends or goes quiet; the reserved peer's own outbound streams
  are unaffected, and an outbound stream to it carries its traffic in that
  direction. `allowlist` remains the default mode, where every admitted
  peer is reserved and the problem does not arise. Resizing the defaults
  or reserving inbound sockets is decided with the switch of the default
  to `open` (D1) and at activation (G20).
- **Handshake monopolisation** by a few source addresses and
  wildcard-certificate peers filling the general share stay with G20 and
  W12, as recorded in the connection-manager design.

## Telemetry

`waddle.s2s.discovery` counts resolutions by `source`: `configured`,
`srv`, `fallback`, `no_service`, `lookup_failed` or `blocked`. A domain
answered from memory is not resolved again and is not counted again. No
domain, name or address is a metric attribute. Rate-limited dials count
as `waddle.s2s.capacity.refusals{resource="dial_rate"}`, refused
verification connections as `resource="verifications"` or
`resource="verification_rate"`.

## Recorded deviations and restrictions

- No A/AAAA fallback after a failed SRV lookup (RFC 6120 §3.2.1 SHOULD).
- A "." target among other targets forbids fallback like a "." alone, and
  so do SRV records none of whose targets is usable.
- A failed A or AAAA lookup with no usable address from the other is a
  failed lookup, retried with backoff, not absence.
- IPv4-mapped and IPv4-compatible AAAA records are refused outright.
- Endpoints are tried one at a time, each within the 5 s connect deadline
  and all within the 20 s negotiation deadline, so a domain whose first
  addresses do not answer may run out of time before its later ones are
  tried; there is no parallel (Happy Eyeballs) connect.
- `see-other-host` ends the attempt and is never followed (D3).
- Discovery loads only with `peer_scope = "public"`.

## Follow-ups

- Discovery against real SRV records, including `_xmpps-server`
  targets, is outside the W10 harness: its peers are on loopback, which
  the address checks refuse for any DNS-derived address, so it reaches
  them through configured endpoints and there is no test-only way around
  those checks. It needs peers at public addresses with published records
  and is decided with activation (G20); until then the stub suite and the
  loopback `SystemDnsLookup` tests are the evidence for discovery.
- W5a and G20: per-sender fairness of the dial rate (above).
- DNSSEC validation, with DANE (D2's later slice).
- NB1 and the per-address limits at the switch to `open` and at G20.
