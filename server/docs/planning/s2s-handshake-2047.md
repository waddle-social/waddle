# Issue 2047: proposed S2S handshake boundary

## Status and scope

Design for the S2S transport under
[ADR-0020](../adrs/0020-independent-community-federation.md) and
[the delivery plan](../../../docs/planning/federation-2047.md). ADR-0020 settles
part of it, as amended below, and its remaining blocking decisions B1–B4 are the
ADR's gates G1–G4. Under the ADR's gate semantics, the slice that implements an
open decision decides it, records it in this document and has it independently
reviewed before that slice merges into the integration branch; the executable
evidence is discharged in the verification phase. This document implements
nothing and does not authorize protected dispatch or production activation.
[Status after ADR-0020](#status-after-adr-0020) records what the ADR settled
and which slice owns each remaining item.

**Amended by ADR-0020 decisions D1–D3, D8 and D9.** Where this document
conflicts with them, they control:

- Peer admission uses the `closed`, `allowlist` and `open` modes of D1. The
  explicit pair configuration below remains the fixture profile. The
  connection manager defines the configuration for the D1 modes, the inbound
  dialback list and the D8 trusted-home list, including how a change reaches
  every replica and closes the streams of a domain that is no longer admitted
  (gate G7). Allowlist, dialback-list and trusted-home entries match exactly;
  a denylist entry also covers every subdomain of its domain, applies in every
  mode and overrides the other lists, and a configuration that also lists a
  domain the denylist covers as allowed, dialback or trusted fails to load.
  All of them, and every stream domain, are compared in the canonical domain
  form that the identity-profile slice defines (gates G1 and G10).
- A leaf whose extended key usage is serverAuth only is accepted in both TLS
  roles, in every trust profile and admission mode (see
  [the verifier design](s2s-verifier-2047.md)).
- The trust profile is chosen per domain. Public WebPKI is the default for
  every domain in every mode; an operator may instead configure a domain with a
  private-CA profile, which is the explicit-anchor profile described below, and
  a public-CA certificate then fails for that domain. This supersedes the rule
  below that only the selected peer's explicit trust anchors are used, except
  for domains configured with a private-CA profile.
- Inbound streams use STARTTLS only, and the initial, pre-TLS stream header
  must carry `from`, so the profile and reference identity are fixed before the
  handshake; a peer that omits it is refused before TLS. Inbound direct TLS
  (XEP-0368) is deferred until a two-stage verification design is reviewed, so
  the `s2s` listener accepts STARTTLS only. Outbound streams may use direct TLS
  for `_xmpps-server` targets.
- Inbound XEP-0220 dialback is permitted under D2's restricted policy, and
  Waddle will implement the outbound dialback roles. This supersedes the
  statement below that the profile has no dialback; multiplexing further
  domains onto a stream remains excluded. `VerifiedPeerStream` records the
  stream's authentication strength, `pkix` or `dialback`, which travels into
  the remote principal and the protected-effect admission witness.
- DNS discovery follows D3, including the blocked address classes checked on
  every DNS-derived address, and lookups use no resolver search list. DNS
  never supplies a reference identity. A remote domain that is either RFC 7622
  IP-address form, another name accepted as an IPv4 address or an unqualified
  single-label name is refused in both directions, as the identity profile's
  canonical domain form classifies it, and a `see-other-host` stream error
  ends the connection attempt; Waddle never follows or sends one.
- Tests are written with the transport slices and executed in the
  verification phase, except the verifier's focused set, which runs when its
  slice completes (D9).

The loopback and `.test` restriction on enabled listeners, endpoints and
domains stays until the discovery slice lifts it through a reviewed change
(gate G6). The native harness keeps loopback endpoints and `.test` domains.

The slice establishes and tears down authenticated, directional domain streams
between two native Waddle processes on isolated test domains. It accepts no
application work, creates no remote account, grants no membership, and takes no
delivery custody. C2S remains WebSocket. No changes to feature advertisements,
Helm exposure, cluster relay, account provisioning or application routing belong
in this slice. A completed handshake proves only its recorded transport checks.

## Source baseline and proposed ownership

Inspected main `e8a3a3d12e0febd811a09551258629a7348742cb`:

| Existing source | Boundary and proposed use |
| --- | --- |
| [`main.rs`](../../crates/waddle-server/src/main.rs) | Captures inherited descriptors before Tokio; preserve this ownership and startup ordering. |
| [`server/lifecycle.rs`](../../crates/waddle-server/src/server/lifecycle.rs) | Acquires only `http`, then asserts inherited listeners consumed. Add conditional `s2s` ownership and join its tasks during shutdown. |
| [`server/config.rs`](../../crates/waddle-server/src/server/config.rs) | Owns XMPP account/MUC/Space domains. Add a separate validated S2S configuration; do not infer remote trust from these local domains. |
| [`server/http.rs`](../../crates/waddle-server/src/server/http.rs) | Uses `axum::serve`; this listener does not terminate S2S TLS. Leave HTTP/C2S serving unchanged. |
| [`server/acme.rs`](../../crates/waddle-server/src/server/acme.rs) | Requests the account-domain certificate and exposes HTTP-01 challenge handling. This is not an S2S certificate provisioning/verification implementation. |
| [`protocol/frame.rs`](../../crates/waddle-xmpp/src/protocol/frame.rs) | Parses complete C2S WebSocket frames with `jabber:client`. Do not feed incremental S2S bytes through this classifier or inject a client namespace. |
| [`iq/dispatch.rs`](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/dispatch.rs) | C2S IQ dispatcher. At main `f77c03814` it forwards results and errors addressed to a local full JID (#2070) and otherwise consumes or ignores them. Do not reuse it as a remote dispatcher. |
| [`route_to_connection/implementation.rs`](../../crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs) | Foreign bare-JID routing is not federation delivery. Leave it outside this slice. |

Proposed new `waddle-server::s2s` owns TCP, rustls configuration, certificate
verification, connection admission, deadlines and shutdown. Proposed
`waddle-xmpp::s2s` owns a bounded streaming XML codec, typed stream headers,
negotiation transitions and typed failures. Names are proposed modules, not
existing APIs. Model namespaces and errors through dedicated types/constants;
build XML with typed builders. Raw bytes remain at the codec/TLS boundary.

Only the connection owner can construct `VerifiedPeerStream`, after both TLS
validation and EXTERNAL authorization complete. Its private fields record the
configured peer ID, exact source/destination `DomainPart`, direction, immutable
trust-generation ID, certificate evidence digest and validity deadline. This is
a domain capability, not a `Session`, local account, grant or delivery receipt.
The established stream is owned by one task; no cloneable sender to an
application dispatcher exists in this slice.

Future ingress must distinguish account JIDs from service identities. A remote
account principal will retain its prepared bare account JID and optional full-JID
routing provenance, plus the verified domain capability. A domain certificate
vouches for its home server, not independent proof that a particular resource is
currently bound. That account/resource assertion remains the home server's
responsibility. Constructing a grant-bearing principal or dispatching protected
work requires the approved identity codec and final authorization fence.

## Test topology and configuration

Two deployments use `home-a.test` and `home-b.test`; their explicitly served
components are `muc.home-a.test`, `spaces.home-a.test`, `muc.home-b.test` and
`spaces.home-b.test`. Each has its own deployment UUID, database credentials,
persistent stores, SpiceDB authority, session/occupant secrets, TLS keys and CA
private material. Only public trust material crosses deployments. No live
Academy/Korora domain, secret, public DNS record or production CA is needed.

The candidate configuration interface is `WADDLE_S2S_ENABLED=false` and, only
when true, `WADDLE_S2S_CONFIG_FILE` naming an operator-owned TOML file. The future
release build keeps this disabled by default; absence of a file does not enable
anything. Disabled mode performs no S2S binds, peer dials or credential reads.
The first implementation rejects enabled non-loopback listeners/endpoints and
non-`.test` domains. Removing that restriction is a separately reviewed scope,
owned by the discovery slice (ADR-0020 gate G6).

The file has these mandatory, typed fields; unknown fields fail validation:

| Field | Meaning |
| --- | --- |
| `generation` | Non-secret UUID identifying this immutable snapshot; not a grant generation. |
| `listen` | Explicit loopback socket address; port zero permitted only in the native fixture. |
| `local_domains` | Exact served domain set, checked against configured local account/components. |
| `credentials` | Exact local domain to certificate-chain/key file mapping. |
| `peers` | Stable peer ID to that peer's exclusive trust-anchor and complete CRL bundle files. |
| `inbound_pairs` | Explicit `(peer, remote_source_domain, local_destination_domain)` tuples. |
| `outbound_pairs` | Explicit `(peer, local_source_domain, remote_destination_domain, endpoint)` tuples. |

Normalize domains once into the identity profile's canonical domain form
(ADR-0020 gate G10). Reject
local/remote overlap, duplicate pairs, contradictory domain-to-peer assignments,
unserved local identities and missing credentials/trust/CRLs. No wildcard trust
entries, suffix matching or implicit parent-domain authority. Maximum 2 peers,
6 local domains and 12 pairs in each direction for this fixture profile.

A pair uses one initiating connection. Its reverse is a separately configured,
independently authenticated connection. A stream authenticated from
`home-a.test` cannot carry `muc.home-a.test` traffic even when both names occur
in its certificate. Components need their own pairs. There is no XEP-0288,
dialback, multiplexed domain authentication or arbitrary relay. ADR-0020 D2
admits restricted dialback, one domain pair per stream; see the amendment
above.

The configured endpoint selects the socket only. It never replaces the remote
destination domain as the certificate reference identity or TLS SNI. The fixture
uses explicit loopback endpoints. DNS discovery, decided in
[the discovery design](s2s-discovery-2047.md), adds no trust and authorizes no
redirect. Unexpected inherited sockets
fail startup. An enabled inherited start requires the named `s2s` descriptor;
a cold start binds it normally. Follow the existing [startup safety rules](../../../docs/planning/startup-safety-2020-2024.md).

## TLS, certificate roles and trust

Waddle terminates STARTTLS in the S2S connection task. No HTTP ingress,
forwarded header, cluster peer identity or TLS-terminating proxy supplies its
identity. A future proxy must pass raw TCP unless a different trust boundary is
reviewed. The receiving side requests and requires the initiator's certificate;
the initiator also validates the receiver. Application bytes cannot cross the
TLS boundary until all checks succeed.

The candidate fixture profile uses TLS 1.3, no early data, no TLS session tickets,
no session cache/resumption and no compression. Every connection is a fresh
handshake. TLS 1.2/general interoperability support is outside this restricted
profile; its relationship to mandatory implementation requirements is blocker B1.

Each deployment issues separate end-entity certificates for its exact served
domains using its private test CA. A certificate used in both connection roles
has both serverAuth and clientAuth EKUs, digital-signature key usage, a non-CA
basic constraint and an exact DNS SAN. This is a fixture issuance choice, not
a claim that all standards-compliant S2S certificates require both EKUs. The
candidate verifier must distinguish the TLS role from the XMPP server identity.

Outbound reference identity is the configured stream `to` domain; inbound
reference identity is the claimed stream `from` domain, which must also select
an allowed inbound pair. Verify certificate path, signatures, current validity,
constraints/key usage, applicable EKUs and revocation before accepting identity.
Use only the selected peer's explicit trust anchors, never a fallback system
root store. Under ADR-0020 D2 this is the private-CA profile configured for a
domain; every other domain uses the public-WebPKI profile, selected before the
handshake. Verify SAN service identity against the reference domain, not the
endpoint IP, SNI alone, subject Common Name or any certificate-supplied redirect.

DNS-ID and `_xmpp-server` SRV-ID verification coverage must be demonstrated;
DNS SAN issuance alone does not prove SRV-ID handling. RFC 6120's identifier
requirements, optional XmppAddr compatibility and RFC 9525's current rules need
a reviewed verifier matrix (B1/B2). Tests must distinguish identity matching
from the separate exact-domain allowlist. Even a valid wildcard or multi-SAN
certificate never expands the configured stream authority.

## Immutable trust, rotation and revocation

Load and validate the complete credential/trust snapshot before binding S2S.
Read files off async workers using the established startup pattern. Limit the
TOML file to 64 KiB, each PEM/CRL file to 1 MiB, and total loaded material to
16 MiB. Reject missing, unreadable, ambiguous or mismatched key/cert material;
keep private key bytes out of arguments, environment variables, logs and test
evidence. No automatic ACME or network AIA/OCSP fetch belongs in this slice.

Use locally supplied, signed, current CRLs covering every non-anchor certificate
in the accepted chain. Missing coverage, unknown status, invalid signatures,
unsupported critical extensions or absent/expired `nextUpdate` fail closed.
Trust-anchor withdrawal uses removal from configuration. Exact CRL support and
freshness semantics in the chosen verifier are blocker B2; accepting an unchecked
chain while this is unresolved is prohibited.

No hot reload: modifying files does not change a running process's snapshot.
Rotation/revocation requires explicit stop, bounded drain, process exit and
restart with a new generation. A fleet operator must confirm all relevant
replicas stopped before claiming the old generation retired. This document
defines no runtime revoke API and no instantaneous remote revocation guarantee.
If replacement material cannot validate, that deployment does not start S2S.
Planned CA rotation can overlap old/new trust in one explicitly reviewed
snapshot; removal still requires retirement of every old process. An emergency
withdrawal stops old-generation connections before any restart.

An active stream expires at the earliest certificate-chain expiration, CRL
`nextUpdate`, or maximum connection age. The 10-minute maximum age is a
fixture-profile value; the connection manager sets and reviews the general
value with its resource model (gate G7). Timer-driven closure applies
even to idle sockets. Recheck validity before successful EXTERNAL completion;
a backward wall-clock jump or inability to establish current time fails the
connection closed. Monotonic deadlines bound lifetimes; wall clock validates
certificates/CRLs. Revocation published after snapshot load becomes effective
only on restart or the freshness deadline, whichever occurs first. The fixture
must make that limitation visible; production revocation policy remains gated.

## Negotiation and error contract

The state machine is: TCP -> initial header -> required STARTTLS -> mutual TLS
-> fresh header -> EXTERNAL -> fresh header -> authenticated handshake-only
stream -> close. Response headers contain a fresh unpredictable stream ID and
`xml:lang`; initial IDs are ignored. On each restart discard prior XML namespace
and negotiation state, retaining only the configured pair and verified TLS
evidence where appropriate. Require the exact same prepared pair across all
headers. No resource binding or XEP-0198 state is created.

After STARTTLS `proceed`, stop plaintext XML processing. Bytes already buffered
beyond that element belong exclusively to the TLS engine, never to a later XML
parser. After SASL success require the new header; pre-success application input
must not become authenticated queued work. B3 requires an executable boundary
proof for segmented/coalesced TCP reads and namespace inheritance.

Advertise only EXTERNAL after acceptable certificate identity and pair checks.
The Waddle initiator acts as its own certificate-verified source domain, including
on component-domain streams, and sends a zero-length initial response as `=`.
It MUST NOT send a same-identity authzid: [RFC 6120 section 6.3.8](https://www.rfc-editor.org/rfc/rfc6120.html#section-6.3.8)
prohibits providing an authorization identity when not acting for another entity.
The pinned XEP-0178 section 3 compatibility recommendation to include an authzid
conflicts with that rule in this case; this design follows the RFC requirement.

The receiver assigns an empty response to the already verified source domain.
For an omitted initial response, issue one empty SASL challenge and accept one
bounded response. A peer's optional nonempty authzid is tolerated only when it
is a prepared domain exactly equal to the certificate-verified, pair-authorized
source; this grants no extra identity and does not cause Waddle to send one.
A different domain, account JID or resource JID receives `invalid-authzid`.
No alternate authorization identity, user lookup, password or fallback mechanism
is supported. Omitted data and the explicit zero-length initial response remain
distinct parser events.

SASL has explicit `AwaitAuth`, `AwaitResponse` and `Authorizing` substates before
success. Process incoming control elements in order. Each accepted `auth` starts
a new exchange generation; cancelling/replacing it invalidates pending checks
and prevents their late completions from emitting success. TLS evidence, domain
pair, byte/rate accounting and connection deadlines survive these transitions.
The following transitions take precedence over the generic unexpected-command
rule; none restarts the XML stream or TLS handshake:

| SASL state and input | Transition and response |
| --- | --- |
| `AwaitAuth` + `auth` | Start an exchange; explicit response enters `Authorizing`, omitted response emits one empty challenge and enters `AwaitResponse`. Validate the requested mechanism and payload with the failure table below. |
| `AwaitResponse` or `Authorizing` + replacement `auth` | Discard the unfinished exchange and process the new requested handshake, per [RFC 6120 section 6.4.2](https://www.rfc-editor.org/rfc/rfc6120.html#section-6.4.2). Do not emit a failure for the discarded exchange. An unsupported replacement mechanism receives `invalid-mechanism`, not generic `policy-violation`. |
| `AwaitResponse` + `response` | Consume the one response and enter `Authorizing`; empty response uses the verified source, nonempty response receives the same authzid checks as initial data. |
| Unauthenticated SASL state + `abort` | Invalidate any unfinished exchange and emit SASL failure `aborted`, per [RFC 6120 section 6.4.4](https://www.rfc-editor.org/rfc/rfc6120.html#section-6.4.4). Return to `AwaitAuth` while the limits below allow another exchange; otherwise close after the required `aborted` response. An abort never authenticates or resets a deadline. |
| Successful current-generation authorization | Emit success exactly once, leave SASL and require the post-SASL stream header. Further SASL traffic is outside an ongoing exchange and closes with `policy-violation`. |

Allow at most three `auth` starts total per TCP connection (initial plus two
replacement/retry starts), and at most three `abort` elements. A fourth `auth`
closes with `policy-violation` because the exchange budget is exhausted, not
because replacement authentication is unsupported. The third abort emits
`aborted` and closes; an abort after the third start also emits `aborted` and
closes because no starts remain. Awaiting another auth cannot extend the
5-second SASL-phase deadline, anchored at the mechanisms offer, or the 20-second
whole-negotiation deadline. One STARTTLS restart and one successful-SASL restart
are the only permitted XML stream restarts. The ordinary terminal SASL failures
below still close: this EXTERNAL-only profile does not retry invalid certificate
or identity evidence as if it were a mistyped user password.

Use the following ordered failure mapping. Errors are typed and contain no
reflected peer XML/JIDs. Send at most one bounded terminal response and close;
if the protocol state cannot safely carry it, close TCP. The `aborted` transition
above is explicitly nonterminal while retry budget and deadlines remain. Do not
send XML inside a failed TLS handshake. Receiving a terminal peer error closes
without replying with another error. The opening response required to carry a
stream error must itself follow RFC 6120; B1 includes malformed initial-header
cases.

| Condition | Proposed wire result and lifetime |
| --- | --- |
| Global/pre-auth capacity unavailable before a safe stream | Close TCP immediately. |
| Malformed XML / forbidden DTD or entity declarations | `not-well-formed` / `restricted-xml` stream error, then close. |
| Wrong stream/content namespace or unsupported stream version | `invalid-namespace` / `unsupported-version`, then close. |
| Missing/invalid domain-only stream header address | `bad-format`, then close. A mandatory pre-TLS `from` is ADR-0020 D2 policy; its absence also receives `bad-format` (see [codec decisions](#codec-decisions-for-g1-and-g3)). |
| Header recipient not served or not the selected destination | `host-unknown`, then close. |
| Source pair not admitted, whichever rule refused it | `policy-violation`, then close (see [decisions after the G4 review](#decisions-after-the-g4-review)). |
| Source identity changes on a later header, or a source spelling the identity profile refuses | `invalid-from`, then close. |
| Application stanza before completed authentication | `not-authorized`, then close. |
| Peer declines/strips STARTTLS or sends another negotiation command | Attempt required STARTTLS if the feature was stripped; refuse any fallback. Unexpected command outside the defined SASL replacement/abort transitions: `policy-violation`; close. |
| TLS/certificate/path/name/revocation failure | TLS alert when available, then TCP close; never EXTERNAL success. |
| Unsupported SASL mechanism | SASL `invalid-mechanism`, then close. |
| Bad SASL base64, invalid UTF-8 or structurally invalid exchange | SASL `incorrect-encoding` for encoding failure, otherwise `malformed-request`, then close. |
| Supplied authzid is not the verified source domain | SASL `invalid-authzid`, then close. |
| Authorization evidence fails at EXTERNAL completion | SASL `not-authorized`, then close. |
| Application stanza lacks valid `from` or `to` | `improper-addressing`, then close; validate addresses before typed parsing discards malformed values. |
| Stanza recipient is outside this stream's exact local destination | `host-unknown`, then close. |
| Stanza sender is outside this stream's exact authenticated source | `invalid-from`, then close. |
| Otherwise valid application stanza, including IQ result/error | `policy-violation`, then close: application dispatch is disabled. Never generate stanza replies or success. |
| Byte/depth/count/rate limit exceeded | `policy-violation`, then close; allocation failure/capacity exhaustion after stream open uses `resource-constraint`. |
| Read/negotiation/idle deadline | `connection-timeout`, then close where safe. |
| Certificate/CRL expiry or connection age reached | `reset`, then close where safe. |
| Normal operator drain | `system-shutdown`, then bounded close; peer stream close gets a normal closing tag. |

The checks for authenticated stanzas run in the table's address/recipient/source
order before the final disabled-dispatch rejection. No stanza is routed, stored,
acknowledged or retried. A terminal stream error is not an application error
reply, so no reverse application stream is needed for this slice.

## Parser, connection and work bounds

These are proposed fixture-profile ceilings, enforced as hard maxima. They are
not measured capacity claims. A future increase needs review and new exhaustion
evidence. They size the two-process fixture and are not the resource model for
ADR-0020's `open` mode, in which the number of peers is attacker-controlled;
the connection manager defines global caps on peers, sockets, handshakes and
per-peer state with an eviction and backoff policy (gate G7). Counters/permits are released on every parse error, timeout and task
cancellation; no unbounded spawned-task or logging queue is allowed.

| Resource | Ceiling / deadline |
| --- | --- |
| Open S2S sockets, all stages | 32 per process; 16 inbound and 16 outbound. |
| Unauthenticated inbound sockets | 8 globally; 2 per source address (ceiling 4), before header parsing, as [the connection-manager design](s2s-connection-manager-2047.md#resource-model) sets them (gate G7). |
| Concurrent expensive handshakes | 4 inbound and 2 outbound TLS handshakes, separately, each direction on its own verifier workers; the per-address and per-peer limits also apply ([connection-manager design](s2s-connection-manager-2047.md#resource-model), gate G7). |
| Established streams | 1 per exact directional pair; a new authenticated stream replaces the incumbent, rate limited (see [decisions after the G4 review](#decisions-after-the-g4-review)). |
| Inbound accept rate | Token bucket 8/second, burst 8 globally; 2/second, burst 4 per source IP. |
| Initial/restarted header or SASL element | 8 KiB including attributes; decoded authzid at most 1 KiB. |
| XML stanza, shape, buffering and decoded tree | Before SASL succeeds, the pre-authentication mode holds every first-level element to 8 KiB; after it, `STREAM_CODEC_LIMITS` applies (512 KiB per stanza, 4,096 elements, depth 32 and the rest), with its memory model, as the [decoding limits of the connection-manager design](s2s-connection-manager-2047.md#decoding-limits-g7) set them (gate G7). Every stage rejects DTDs, external entities and unsupported encodings and bounds input incrementally before a tree is built; whitespace counts toward the input rate. |
| TLS handshake buffering | 128 KiB per connection, chain at most 8 certificates and 64 KiB encoded; demonstrate library enforcement under B2. |
| Output buffer | 16 control frames and 64 KiB per connection; first exhausted bound closes. No application queue. |
| TCP connect / initial header / TLS / each expected stream restart | 5 seconds each; 20 seconds total through authenticated post-SASL header. Timers do not reset on partial bytes. |
| Whole SASL phase | 5 seconds from mechanisms offer, including challenges, replacements and abort/retry; never renew this or the total negotiation deadline. |
| SASL exchanges / aborts / XML stream restarts | 3 auth starts total, 3 abort elements total; at most the expected post-TLS and post-success headers. Replacements/aborts never add a stream restart. |
| Established incomplete element / idle | 5 seconds of time the server is willing to read, at most 30 seconds per element / 300 seconds, as [the connection-manager design](s2s-connection-manager-2047.md#deadlines) sets them (gate G7); continuous whitespace cannot avoid the element/rate limits. |
| Input rate | 64 KiB/second, burst 128 KiB per socket; 512 KiB/second, burst 1 MiB globally; configurable down to 32 KiB/second ([connection-manager design](s2s-connection-manager-2047.md#resource-model), gate G7). |
| Terminal write and close / operator drain | 1 second / 5 seconds, capped by the process's remaining drain budget. |
| Outbound attempts | Made on demand through the connection manager, one in progress per directional pair, with per-domain backoff after a failure (gate G7); no autonomous retry. |

Bound source-IP accounting to the admitted socket set plus at most 256 expiring
rate-bucket entries with a 60-second inactive expiry; reject new buckets while full. Source-domain claims cannot
create unbounded peer state. Crypto/certificate work runs behind its semaphore;
blocking work must have bounded queued jobs and must not retain resources after
connection cancellation beyond a documented verifier bound (B2). S2S has its own
permits and tasks: no local-session registry or grant/database lock is held while
waiting for a remote peer. The two-process tests must prove unrelated C2S progress.

## Blocking architecture decisions

| ID | Required resolution |
| --- | --- |
| B1: standards profile | Record RFC 6120/7590/7622/9325/9525 updates and errata disposition; reconcile TLS 1.3-only restriction, DNS-ID/SRV-ID/XmppAddr treatment, IDNA/preparation, remaining SASL conformance and exact malformed-header/error behavior. The authzid precedence and replacement/abort rules above resolve those specific review findings; other B1 questions remain open. Identify intentional pilot restrictions without claiming general compliance. |
| B2: concrete TLS verifier | Select exact rustls/webpki/parser versions and APIs; prove server certificate identity verification in both TLS roles, SRV-ID support, EKU/chain constraints, signed CRL coverage/freshness and enforceable pre-verification memory/CPU limits. No permissive custom verifier or unchecked fallback. |
| B3: codec and boundary | Select streaming parser/version and prove namespace inheritance, UTF-8 segmentation, element/attribute/depth accounting and STARTTLS/SASL restart buffer ownership with executable adversarial cases. |
| B4: independent review | An independent reviewer challenges this design with its B1–B3 decisions recorded, and any B1–B3 evidence then available. Unresolved blocking findings pause the merge of the connection manager, the first slice that accepts sockets, into the integration branch. This reviews the design; the implementation head is reviewed in the close-out review (ADR-0020 gate G19). |

These remain blocking decisions; an implementation that compiles waives none of
them. Under ADR-0020's gate semantics, each is decided by its owning slice and
reviewed before that slice merges into the integration branch. The executable
proofs that B2 and B3 call for are evidence, rather than preconditions for
writing the code: under D9, B2's run with the verifier's focused set when its
slice completes and again in verification, and B3's run in verification. The authorization ID codec
(gate G9), the identity profile (G10) and the host-grant fence (G11–G17) remain
separate gates for remote grants and protected dispatch even if B1–B4 pass.
No two-process handshake result relaxes those gates or the production window
and runbook requirement. Remote IQ correlation, delivery custody/deduplication,
ordering, unknown outcomes and recipient-set preservation belong to S2S
routing (G8).

### Status after ADR-0020

ADR-0020's accepted decisions settle part of B1 and B2. Nothing in this table
is implemented or proven. Slice names refer to
[the delivery plan](../../../docs/planning/federation-2047.md), whose gate
table also names the verification step for each item.

| ID | Settled by ADR-0020 | Remaining items, owner and gate |
| --- | --- | --- |
| B1 | D2: TLS on every stream; PKIX and SASL EXTERNAL as the primary path; serverAuth-only certificates in both roles; per-domain trust profiles; inbound STARTTLS only, with a mandatory pre-TLS `from`; inbound dialback policy and outbound dialback roles; one domain pair per stream. D3: discovery, the source domain as the only reference identity, blocked address classes, lookup failure versus absence, IP-address, numeric IPv4 and single-label domains, and redirects. D1: admission modes, list matching and denylist precedence. | Codec slice (W4a): header and stream-error behavior, including the error for a missing pre-TLS `from`, and SASL conformance, recorded in [codec decisions](#codec-decisions-for-g1-and-g3). Verifier slice (W4b): updates and errata disposition; TLS version profile (RFC 9325 section 3.1.1 requires TLS 1.2 support, while this fixture is TLS 1.3 only); DNS-ID, SRV-ID, XmppAddr and wildcard matrix; decided as recorded in [the verifier design](s2s-verifier-2047.md#verifier-decisions-for-g1-and-g2), where the profile admits TLS 1.2 and TLS 1.3. Identity-profile slice (W5i): the canonical domain form and IDNA processing that reference identities and admission lists use. Gate G1. The dialback wire profile and the classification of certificate outcomes are not part of B1; the dialback slice (W4d) decides them under gate G5. |
| B2 | D2: per-domain profiles with public WebPKI by default; serverAuth-only EKU accepted; hard CRL checks only for configured private CAs. | Verifier slice (W4b): concrete verifier for both profiles; revocation handling for public chains; algorithm intersection; parser and CRL ceilings; cancellation and resource bounds; decided as recorded in [the verifier design](s2s-verifier-2047.md#verifier-decisions-for-g1-and-g2). Evidence: the verifier's focused set when the slice completes, then two-process evidence in verification. Gate G2. |
| B3 | Nothing. | Codec slice (W4a): decided as recorded in [codec decisions](#codec-decisions-for-g1-and-g3); evidence in verification. Gate G3. |
| B4 | Nothing; accepting the ADR is not this review. | Independent review of this design with its recorded B1–B3 decisions, including the codec decisions and the identity profile's canonical domain form, before the connection manager (W4c) merges into the integration branch. Gate G4. |

### Codec decisions for G1 and G3

The codec slice (W4a) decided the following for the `waddle-xmpp::s2s` codec
and negotiation machine. They are recorded here for the G4 review; nothing in
this section is executed evidence. Their evidence is the codec's suites,
`crates/waddle-xmpp/tests/s2s_codec.rs` and
`crates/waddle-xmpp/tests/xep0178_s2s_negotiation.rs`, which are compiled and
run in verification step 2.

**Parser and restart buffers (G3).** The decoder drives `rxml`'s
non-validating `RawParser`, the version the vendored `minidom` uses, and
resolves namespaces itself, so every ceiling is checked against bytes handed
to the parser before state is retained. It parses one first-level item per
call and records the exact input offset where that item ended. After
`<proceed/>`, the bytes that followed are handed to the TLS engine and never
reach an XML parser; in the receiving role that handoff is always empty,
because input the initiator sends after `<starttls/>` without waiting for
`<proceed/>` closes the stream with `policy-violation` (RFC 6120 section
5.4.3.1). Bytes received between SASL success and the post-SASL stream header
are discarded and close the stream with `policy-violation`. Each read handed
to the decoder is at most the buffered-input ceiling minus the stanza ceiling
(64 KiB with the pilot limits).

**Codec limits.** The codec's pilot limits (`CodecLimits::PILOT`, which the
codec's own tests use) are an 8 KiB header, a 64 KiB stanza, depth 32, 32
attributes per element, 1,024 elements and 8 KiB of attributes per stanza
and 128 KiB buffered; the connection manager uses `STREAM_CODEC_LIMITS`
after SASL instead ([connection-manager design](s2s-connection-manager-2047.md#decoding-limits-g7),
gate G7). The pilot limits add: 8 KiB for a first-level
STARTTLS, SASL or dialback element; 8 KiB for one name or attribute value
while the parser assembles it; 1 KiB for a declared namespace name; and
128 KiB for the strings retained by the decoded tree of one first-level
element, charged as the tree is built because decoding copies an inherited
namespace name into every element. By the codec's own accounting the worst
case is about 1.7 MiB resident per connection, dominated by element node
structure that no byte ceiling charges; this is an analysis, not a
measurement.

**Header checks.** The receiving role checks each stream header in this order
and closes with the first failing condition:

| Check | Stream error |
| --- | --- |
| `version` absent or below 1.0 | `unsupported-version` |
| `to` absent, empty, not a JID or not domain-only | `bad-format` |
| `to` spelled in a form the identity profile refuses | `host-unknown` |
| `to` names a formerly served domain | `host-gone` |
| `to` names a domain that is not served | `host-unknown` |
| `from` absent, empty, not a JID or not domain-only, on any header including the pre-TLS one | `bad-format` |
| `from` spelled in a form the identity profile refuses | `invalid-from` |
| A later header changes the destination | `host-unknown` |
| A later header changes the source | `invalid-from` |
| The source and destination pair is not admitted | `policy-violation` |

A certificate verified for a domain other than the header's `from` closes
with `invalid-from`. The initiating role checks the response header in the
same spirit: `unsupported-version` for a missing or old version; `bad-format`
for a missing or invalid `from` or `to` or a missing stream ID; `invalid-from`
when `from` is not the destination; `host-unknown` when `to` is not the
source.

**Decoding and stream errors.** Comments, processing instructions, DTDs and
other markup declarations, and entity references other than the predefined
ones, are `restricted-xml`; so is an XML declaration naming an encoding other
than UTF-8, which the parser cannot distinguish from other forbidden
declarations. Invalid UTF-8 is `unsupported-encoding`. A stream namespace
other than `http://etherx.jabber.org/streams`, a content namespace other than
`jabber:server`, or any namespace other than the stream and dialback
namespaces declared on the header, is `invalid-namespace`; a stream prefix
other than `stream` or a prefixed `jabber:server` or `jabber:client` first-level
element is `bad-namespace-prefix`. Non-whitespace text between first-level
elements, a root element other than `stream`, and a malformed `version` or
`xml:lang` are `bad-format`. Exceeding any ceiling is `policy-violation`. A
`jabber:client` stanza is `invalid-namespace`; a stanza before authentication
is `not-authorized`; a negotiation element out of sequence is
`policy-violation`. On an established receiving stream, a stanza without a
valid `to` and `from` is `improper-addressing`, a recipient outside the
stream's destination is `host-unknown`, a sender outside its source is
`invalid-from`, any other stanza is `policy-violation` while application
dispatch is disabled, a negotiation or dialback element is
`policy-violation`, and only a first-level element that is none of these is
`unsupported-stanza-type`. Streams are directional, so a first-level element
received on an established initiating stream is `policy-violation` once it
passes the namespace checks above; a `jabber:client` stanza there is still
`invalid-namespace`.

When the receiving role has no response header open, it opens one before the
error, with `from` set to the last served recipient or the configured default
domain, no `to` and a fresh stream ID (RFC 6120 sections 4.9.1.2 and
4.9.1.3). No XML is sent during the TLS handshake. Stream errors and SASL
failures carry only the defined condition: no text, no application-specific
element and nothing reflected from the peer. A received stream error closes
the stream without a reply; an unknown condition is treated as
`undefined-condition` (RFC 6120 section 4.9.2). The codec never sends
`see-other-host`, and a received one closes the stream like any other peer
error; it is not followed. Timeouts close with `connection-timeout`,
certificate, CRL or age expiry with `reset`, a drain with `system-shutdown`,
and resource exhaustion after the stream opened with `resource-constraint`.

**SASL conformance.** The receiving role offers only EXTERNAL, and only after
TLS verified a certificate for the header's `from`. A SASL element with child
elements is `malformed-request`; base64 that does not decode is
`incorrect-encoding`; data longer than the authzid ceiling is
`malformed-request`. An omitted initial response receives one empty challenge,
and `=` is an empty response. An authzid that is not UTF-8 is
`incorrect-encoding`, and one that is not the verified source domain is
`invalid-authzid`. An unsupported mechanism, including in a replacement
`auth`, is `invalid-mechanism`, and the stream closes. Authorization is an
explicit asynchronous decision by the connection owner, tagged with the
exchange it belongs to, so a decision for a replaced or aborted exchange is
ignored; a denial is `not-authorized`. The `auth` and `abort` budgets follow
the SASL table above. A received SASL failure with an unknown condition is
treated as `not-authorized` (RFC 6120 section 6.5). The initiating role sends
`=` with no authzid, requests STARTTLS whether or not it was advertised,
closes on a TLS `failure`, closes with `policy-violation` unless TLS verified
the receiving server's certificate for the destination, refuses a post-TLS
feature list that still offers STARTTLS or a final feature list that offers
STARTTLS or SASL, and accepts SASL success only without additional data.

**Dialback.** Received dialback elements, a receiving-side TLS peer without a
usable certificate, and an initiating side whose peer offers no EXTERNAL or
refuses it with `not-authorized` are explicit decision points. While
dialback is disabled each closes (`policy-violation`, or the SASL failure
for a refused EXTERNAL); the dialback slice decides their behavior under
gate G5 ([the dialback design](s2s-dialback-2047.md)).

**Carried to the connection manager (G7).** Implemented and recorded in
[the connection-manager design](s2s-connection-manager-2047.md). The codec builds the full tree of
a first-level stanza, up to the stanza ceilings, before the negotiation
refuses it with `not-authorized`. The connection manager adds a
pre-authentication decoder mode: until SASL succeeds, a first-level element
outside the stream, TLS, SASL and dialback namespaces is refused as soon as
its head closes, before any tree is built, and negotiation elements keep the
control-element ceiling. It also owns the deadlines, rate limits, per-address
and global socket caps, the verifier-backed TLS peer, the certificate recheck
before authorization, `VerifiedPeerStream`, and capacity planning that uses
the resident-memory figure above.

### Decisions after the G4 review

The independent G4 review of this design did not pass. The owner decided
the following on 2026-11-14, and the connection-manager slice (W4c)
implemented them in the codec; the resource-model decisions of the same
review are recorded in
[the connection-manager design](s2s-connection-manager-2047.md). Nothing
here is executed evidence; the evidence is
`crates/waddle-xmpp/tests/s2s_stream_addresses.rs`,
`crates/waddle-xmpp/tests/s2s_pre_authentication.rs` and the updated
`crates/waddle-xmpp/tests/xep0178_s2s_negotiation.rs`.

**Internationalized domains (B1).** A stream header's `from` and `to` are
read from the raw attribute text into the identity profile's
`CanonicalDomain` (`CanonicalDomain::parse_exact`), never through JID
preparation: Nameprep folds some spellings (for example `ß` to `ss`) and,
with its Unicode 3.2 tables, refuses IDNA2008 labels made of later code
points (Tifinagh, N'Ko, Glagolitic and others) whose A-labels it accepts,
so two Waddle deployments could not reach each other under such a name. A
value is domain-only when it contains neither `@` nor `/`. ASCII uppercase
letters are mapped to lowercase before the domain is read, which joins
nothing DNS keeps apart; any other spelling that preparation would change,
such as a width, compatibility or non-ASCII case form, is refused. The same
reading applies to an authorization identity in SASL EXTERNAL. A U-label
and an A-label spelling of one domain are the same address, and every pair,
admission and TLS-peer comparison uses the canonical form; `TlsPeer`,
`InboundAdmission`, `InitiatingConfig`, `AuthorizationRequest` and
`NegotiatedStream` carry `CanonicalDomain`. A refused spelling closes the
stream with an explicit error: `invalid-from` for `from` and
`host-unknown` for `to`, in both roles (the table above). Emitted headers
write U-labels, as RFC 7622 §3.2.1 asks of an address presented to a peer;
stanza addresses sent to peers are U-labels too
([routing design](s2s-routing-2047.md)). A stanza's `to` and `from` on an established stream are read with
the identity profile's address parser (`identity::parse_jid`), also without
JID preparation, compared with the stream's pair, and handed on rewritten
in canonical form.

**Direct TLS.** Outbound XEP-0368 direct TLS, for `_xmpps-server` targets
and configured direct-TLS endpoints, is a direct-TLS entry to the
initiating machine (`TlsEntry::DirectTls`): the first step is the TLS
handshake, with no plaintext header and no `<starttls/>`, the Server Name
Indication is the remote domain and the `xmpp-server` ALPN protocol is
offered; everything after TLS is as for STARTTLS, including the refusal of
a STARTTLS offer. There is no inbound direct TLS. See
[the discovery design](s2s-discovery-2047.md#direct-tls-xep-0368).

**Admission refusals before TLS** receive `policy-violation`, whichever rule
refused the pair (the table above). A probe can still tell an admitted
domain from a refused one by whether STARTTLS is offered; that disclosure
of list membership is accepted as low risk.

**Pre-authentication decoding.** Besides the rules carried to the
connection manager below, the control-element ceiling also applies to
stream-namespace elements (features and stream errors) before SASL
succeeds, and a connection whose first bytes are not XML, such as a TLS
ClientHello sent to the STARTTLS port, closes without any XML
(`CloseReason::NotXml`).

**Peer features.** The initiating role reads only whether a features
element offers STARTTLS and whether it offers the SASL EXTERNAL mechanism;
unknown, malformed or unrelated children are ignored instead of failing the
stream with `bad-format`.

**Error responses.** When the receiving server's response header fails the
initiating role's checks, for example because it carries `from=''` as
RFC 6120 §4.9.1.3 allows a server that cannot serve the requested domain,
the next element decides: a stream error from the peer closes the stream
with that error, which the connection manager classifies for backoff (for
example `host-unknown`), and anything else closes it with the condition the
header check found.

**Dialback namespace.** `StreamHeader::declare_dialback` binds the
`jabber:server:dialback` namespace to the `db` prefix on an emitted header
where a dialback role applies to the stream, and reports the declaration
on a received header, which advertises traditional dialback. The decision
points where dialback could follow implement the XEP-0220 roles where
configuration enables them and refuse dialback otherwise; see
[the dialback design](s2s-dialback-2047.md).

**Trusted homes on the public profile (D8).** A domain on the trusted-home
list may authenticate under the public-WebPKI profile; an operator who
wants a trusted home to count only on its own CA lists it in
`admission.require_private_ca` with a private-CA profile. Public chains are
not checked for revocation (verifier design, "Revocation of public
chains"), so a trusted home's compromised key keeps its privileged
authority until its certificate expires, at most the public leaf validity
ceiling, unless the operator denylists the domain or removes it from the
trusted-home list and restarts. On that profile any publicly trusted CA can
issue a certificate for the trusted home's domain, so a mis-issued
certificate, or one obtained by whoever gains control of the domain (an
expired registration, a hijacked DNS zone or registrar account), carries
the same authority for its validity. This residual risk is recorded in
[ADR-0020 D8](../adrs/0020-independent-community-federation.md#decision-remote-authority-and-reassigned-jids-d8)
and accepted as part of the activation decision (gate G22).

**Recorded deviations and restrictions (B1).**

- RFC 6120 §13.8.4 names `TLS_RSA_WITH_AES_128_CBC_SHA` as mandatory to
  implement. It is not offered: RFC 9325 §4.2.1 recommends against static
  RSA key transport and CBC suites, and rustls implements neither.
- A receiving server ignores the TLS Server Name Indication of an inbound
  connection and selects the local domain and its certificate from the
  stream header's `to` (RFC 7590 §3.5 and RFC 6120 allow either). The
  header is read before the handshake on a STARTTLS connection, which is
  the only kind accepted.
- A stream header without `version` is refused with `unsupported-version`
  (RFC 6120 §4.7.5 reads it as version 0.9, which this profile does not
  speak), and one without `to` with `bad-format`. Both are restrictions of
  this profile.
- A duplicate outbound stream for a directional pair replaces the
  existing one, which closes with `conflict`, rate limited as the
  connection-manager design records; the earlier rule above, which refused
  the duplicate and kept the incumbent, is superseded.
- Inbound, a directional pair may hold several streams, as RFC 6120
  allows: a pooled or clustered peer, such as ejabberd with
  `max_s2s_connections` above one, opens several, and up to a configured
  number (4 by default) are kept side by side and each carries its stanzas
  (owner decision of 2026-11-18,
  [connection-manager design](s2s-connection-manager-2047.md#streams-per-pair)).
  The harness slice (W10) tests clustered ejabberd with
  `max_s2s_connections` above one.
- ASCII uppercase letters in a stream header address or a SASL
  authorization identity are mapped to lowercase, more liberal than the
  identity profile's exact-spelling rule and safe because DNS does not
  distinguish ASCII case. Other spellings that RFC 7622 preparation would
  map (width, compatibility and non-ASCII case forms) are refused instead
  of being folded.
- A-labels are accepted in received headers and stanza addresses and read
  as the same domain as their U-labels, although RFC 7622 §3.2.1 says a
  prepared domainpart contains no A-labels; emitted headers use U-labels.
- ASCII uppercase letters in the domainpart of a stanza's `to` or `from`
  on an established stream are mapped to lowercase as in stream headers,
  and the stanza is handed on with the canonical lowercase domain. The
  localpart and resourcepart are read exactly. Any other address the
  identity profile refuses closes the stream with `improper-addressing`.
  A syntactically valid address in a spelling the profile refuses (an
  uppercase localpart, a width or compatibility form) is read through
  RFC 7622 preparation only to compare its domain with the pair and to
  address a reply, and the stanza is handed to the connection owner as
  malformed; the S2S router answers it with a `jid-malformed` stanza error
  and the stream stays open
  ([routing design](s2s-routing-2047.md#ingress-g10)). Stream errors stay
  for an absent, empty or unparseable address and for a domain outside the
  stream's pair, and for a refused spelling on a stream whose owner does
  not take stanzas.
- Whitespace (or U+FEFF) before a stream header, initial or restarted, is
  refused with `not-well-formed`, although XML 1.0 allows whitespace before
  the root element of a document without an XML declaration. The harness
  slice (W10) confirms that Prosody and ejabberd send none.
- On the receiving side, a failed SASL EXTERNAL exchange closes the
  stream (the initiating side may continue with dialback after a
  `not-authorized` failure, see the dialback design). RFC 6120 §6.4.5 says
  the receiving entity SHOULD allow a reasonable number of retries where
  appropriate for the mechanism; a certificate that failed once fails
  again, so this profile allows none. Replacement and abort within the
  exchange budget are the only second attempts.

## Acceptance evidence required of the future implementation

Native Rust suites include dedicated XEP-0178 parser/negotiation coverage and a
real two-Waddle-process lane. No Docker, shared deployment database/secret,
accept-all TLS verifier or mocked transport is acceptable. Reuse lifecycle ideas
from [the native experiment fixture](../../experiments/native-fixture/src/fixture.rs),
but do not treat the authorization experiments built on it as proof of
federation correctness.
[The existing WebSocket TestServer](../../crates/waddle-ws-test-support/src/lib.rs)
forces fixed-account provisioning and `ws://`; it is not F5 acceptance as-is.
The new two-process fixture must use independent production SpiceDB authorities,
normal account authentication and actual WSS for its local-progress assertions.
Test-only CA material and loopback endpoint mapping must still execute the real
certificate/name/revocation checks. Missing binaries/services fail the required
lane, not skip it. Every connection records redacted generation and terminal
outcome, never private keys, tokens or stanza bodies.

1. Establish each allowed account/component pair in both directions. Assert
   exact source/destination authority, fresh TLS/EXTERNAL after reconnect and no
   bound local session, permission lookup, application effect or delivery receipt.
2. Reject unknown CA, missing certificate, bad signature, expired/not-yet-valid
   chain, wrong EKU, CA-as-leaf, SAN mismatch, Common Name-only identity, missing
   or stale CRL, revoked leaf/intermediate and incorrect SRV service identity.
   Exercise valid multi-SAN/wildcard identity matching separately from rejection
   of an unconfigured component/alias, with no parent-domain inheritance.
3. Attack pre/post-TLS and post-SASL header changes, wrong recipient, spoofed
   sender, relay attempts, authzid mismatch, omitted/empty responses, bad base64,
   mechanism downgrade, stripped STARTTLS, unsolicited bind/SM and stale
   authenticated bytes retained across restarts. Assert that Waddle's own
   EXTERNAL auth contains `=` and no authzid, while matching optional peer
   authzids pass only the existing certificate/pair check. Replace unfinished
   exchanges both before response and during pending authorization; assert no
   stale success, and `invalid-mechanism` for an unsupported replacement. Abort
   at each unauthenticated SASL substate and assert `aborted`, permitted restart
   within budget, and bounded closure at the specified limits. Exercise the
   fourth auth, third abort and repeated partial/control inputs near the SASL
   and total deadlines; assert no timer, counter, XML restart or TLS reset.
   Assert the exact transition and failure tables.
4. Fragment every transition and coalesce plaintext/TLS/SASL boundary bytes.
   Test inherited namespace changes, invalid UTF-8, DTD/entity attempts, malformed
   addresses, deeply nested XML, huge attributes, oversized/incomplete stanzas,
   slowloris, whitespace floods and half-open sockets. Assert bounded resident
   resources and admission permits returned after cancellation.
5. Exhaust pre-auth and per-peer sockets/crypto/output/rate limits while normal
   WSS clients on each deployment complete local IQ requests within a recorded
   2-second fixture deadline. The test must assert progress, not just process
   survival. Distinguish policy rejection from transport unavailability.
6. Restart A while B stays live, then reverse. Rotate leaf and CA material,
   withdraw old trust, advance certificate/CRL deadlines and interrupt drains.
   Prove no old-generation connection survives the confirmed process exit;
   mutated files alone must not silently change the active snapshot. Invalid
   replacement material refuses S2S startup. Demonstrate the documented bounded
   revocation window without claiming immediate remote revocation.
7. Confirm default-off cold and inherited startup, no unexpected dials, disabled
   production exposure/advertisement, finite shutdown and zero application
   acceptance for every message/presence/IQ type, including result/error.

Record commit, dependency versions, test-domain configuration, public certificate
fingerprints, trust generations, exact command/results, resource observations
and passed/failed/unverified cases. Future Rust changes require focused suites,
workspace nextest, doctests, formatting and Clippy with `-D warnings` per
[server guidance](../../AGENTS.md). A document/link check now establishes none
of those runtime claims.

## Protocol sources and current review evidence

The repository has no `xeps/` checkout at the inspected base. Reviewed the
[official XEP-0178 XML, version 1.2](https://github.com/xsf/xeps/blob/4132c531487fe9450ae90b905b8df950d0d478b2/xep-0178.xml)
at XSF commit `4132c531487fe9450ae90b905b8df950d0d478b2`, especially its S2S
certificate/EXTERNAL sequence. Reviewed relevant sections of
[RFC 6120](https://www.rfc-editor.org/rfc/rfc6120.html) (streams, TLS/SASL,
address validation and certificates),
[RFC 7590](https://www.rfc-editor.org/rfc/rfc7590.html) (TLS requirements),
[RFC 9325](https://www.rfc-editor.org/rfc/rfc9325.html) (TLS security) and
[RFC 9525](https://www.rfc-editor.org/rfc/rfc9525.html) (service identity).
[RFC 7622](https://www.rfc-editor.org/rfc/rfc7622.html) preparation and the
complete updates/errata/verifier matrix are still required by B1; this proposal
is not a completed conformance audit. XEP examples do not override XML grammar.

This change contains only the proposed document. No implementation tests,
certificate experiment, live DNS/TLS verification, authority proof, deployment
or activation has been performed by this design change.
