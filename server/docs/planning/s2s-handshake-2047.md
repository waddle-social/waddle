# Issue 2047: proposed S2S handshake boundary

## Status and scope

Proposed, for independent architecture review. This document selects a candidate
first transport slice under [ADR-0020](../adrs/0020-independent-community-federation.md)
and [the delivery plan](../../../docs/planning/federation-2047.md). It does not
approve F0, resolve the identity or grant-authority gates, implement transport,
authorize protected dispatch, or authorize production activation. Blocking
questions below must be resolved before transport implementation. Changes to
ADR-0020, including the separate authority work in PR #2067, remain separate.

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
| [`iq/dispatch.rs`](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/dispatch.rs) | Consumes results/errors before generic forwarding. Do not reuse it as a remote dispatcher. |
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
non-`.test` domains. Removing that restriction is a separately reviewed scope.

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

Normalize domains once using the selected XMPP address preparation rules. Reject
local/remote overlap, duplicate pairs, contradictory domain-to-peer assignments,
unserved local identities and missing credentials/trust/CRLs. No wildcard trust
entries, suffix matching or implicit parent-domain authority. Maximum 2 peers,
6 local domains and 12 pairs in each direction for this fixture profile.

A pair uses one initiating connection. Its reverse is a separately configured,
independently authenticated connection. A stream authenticated from
`home-a.test` cannot carry `muc.home-a.test` traffic even when both names occur
in its certificate. Components need their own pairs. There is no XEP-0288,
dialback, multiplexed domain authentication or arbitrary relay.

The configured endpoint selects the socket only. It never replaces the remote
destination domain as the certificate reference identity or TLS SNI. The fixture
uses explicit loopback endpoints, so DNS discovery is deferred; implementing it
later must not add trust or authorize redirects. Unexpected inherited sockets
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
root store. Verify SAN service identity against the reference domain, not the
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
`nextUpdate`, or 10-minute maximum connection age. Timer-driven closure applies
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
The initiator sends its exact source domain as the base64 authorization identity;
the receiver also accepts `=` as the explicit empty initial response and assigns
the already verified source domain. For an omitted initial response, issue one
empty SASL challenge and accept one bounded response. Empty means the source
identity; a nonempty prepared domain must equal it. No alternate authorization
identity, user lookup, password or fallback mechanism is supported.

Use the following ordered failure mapping. Errors are typed and contain no
reflected peer XML/JIDs. Send at most one bounded terminal response and close;
if the protocol state cannot safely carry it, close TCP. Do not send XML inside
a failed TLS handshake. Receiving a terminal peer error closes without replying
with another error. The opening response required to carry a stream error must
itself follow RFC 6120; B1 includes malformed initial-header cases.

| Condition | Proposed wire result and lifetime |
| --- | --- |
| Global/pre-auth capacity unavailable before a safe stream | Close TCP immediately. |
| Malformed XML / forbidden DTD or entity declarations | `not-well-formed` / `restricted-xml` stream error, then close. |
| Wrong stream/content namespace or unsupported stream version | `invalid-namespace` / `unsupported-version`, then close. |
| Missing/invalid domain-only stream header address | `bad-format`, then close; mandatory pre-TLS `from` is an explicit restricted-profile policy, subject to B1. |
| Header recipient not served or not the selected destination | `host-unknown`, then close. |
| Source pair not allowed, or source identity changes | `invalid-from`, then close. |
| Application stanza before completed authentication | `not-authorized`, then close. |
| Peer declines/strips STARTTLS or sends another negotiation command | Attempt required STARTTLS if the feature was stripped; refuse any fallback. Unexpected command: `policy-violation`; close. |
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
evidence. Counters/permits are released on every parse error, timeout and task
cancellation; no unbounded spawned-task or logging queue is allowed.

| Resource | Ceiling / deadline |
| --- | --- |
| Open S2S sockets, all stages | 32 per process; 16 inbound and 16 outbound. |
| Unauthenticated inbound sockets | 8 globally; 4 per source IP, before header parsing. |
| Concurrent expensive handshakes | 4 globally; per-IP and peer limits also apply. |
| Established streams | 1 per exact directional pair; reject duplicate, do not evict incumbent. |
| Inbound accept rate | Token bucket 8/second, burst 8 globally; 2/second, burst 4 per source IP. |
| Initial/restarted header or SASL element | 8 KiB including attributes; decoded authzid at most 1 KiB. |
| XML top-level stanza | 64 KiB raw UTF-8, bounded incrementally before DOM construction. |
| XML shape | Depth 32 below stream root; 32 attributes per element; 1,024 elements per stanza; 8 KiB total attribute bytes per stanza. |
| XML buffering | 128 KiB maximum per connection; reject DTDs/external entities and unsupported encodings; bounded whitespace counts toward rate/deadline. |
| TLS handshake buffering | 128 KiB per connection, chain at most 8 certificates and 64 KiB encoded; demonstrate library enforcement under B2. |
| Output buffer | 16 control frames and 64 KiB per connection; first exhausted bound closes. No application queue. |
| TCP connect / initial header / TLS / each restart or SASL step | 5 seconds each; 20 seconds total through authenticated post-SASL header. Timers do not reset on partial bytes. |
| Established incomplete element / idle | 5 seconds / 30 seconds; continuous whitespace cannot avoid the element/rate limits. |
| Input rate | 64 KiB/second, burst 128 KiB per socket; 512 KiB/second, burst 1 MiB globally. |
| Terminal write and close / operator drain | 1 second / 5 seconds, capped by the process's remaining drain budget. |
| Reconnect | One attempt per configured pair at startup; no autonomous retry in this slice. Fixture restarts initiate fresh attempts. |

Bound source-IP accounting to the admitted socket set plus at most 256 expiring
rate-bucket entries with a 60-second inactive expiry; reject new buckets while full. Source-domain claims cannot
create unbounded peer state. Crypto/certificate work runs behind its semaphore;
blocking work must have bounded queued jobs and must not retain resources after
connection cancellation beyond a documented verifier bound (B2). S2S has its own
permits and tasks: no local-session registry or grant/database lock is held while
waiting for a remote peer. The two-process tests must prove unrelated C2S progress.

## Blocking architecture decisions

| ID | Required resolution before implementation |
| --- | --- |
| B1: standards profile | Record RFC 6120/7590/7622/9325/9525 updates and errata disposition; reconcile TLS 1.3-only restriction, DNS-ID/SRV-ID/XmppAddr treatment, IDNA/preparation, SASL empty/omitted responses and exact malformed-header/error behavior. Identify intentional pilot restrictions without claiming general compliance. |
| B2: concrete TLS verifier | Select exact rustls/webpki/parser versions and APIs; prove server certificate identity verification in both TLS roles, SRV-ID support, EKU/chain constraints, signed CRL coverage/freshness and enforceable pre-verification memory/CPU limits. No permissive custom verifier or unchecked fallback. |
| B3: codec and boundary | Select streaming parser/version and prove namespace inheritance, UTF-8 segmentation, element/attribute/depth accounting and STARTTLS/SASL restart buffer ownership with executable adversarial cases. |
| B4: independent review | An independent reviewer must challenge this exact design/head and its resolved B1-B3 evidence. Unresolved blocking findings pause transport implementation. |

These are blocking decisions, not work deferred behind an allegedly ready
implementation. F1 identity encoding and F3 authority/effect fencing remain
separate blockers for remote grants and protected dispatch even if B1-B4 pass.
No two-process handshake result relaxes those gates or the production window and
runbook requirement. Remote IQ correlation, delivery custody/deduplication,
ordering, unknown outcomes and recipient-set preservation require later slices.

## Acceptance evidence required of the future implementation

Native Rust suites include dedicated XEP-0178 parser/negotiation coverage and a
real two-Waddle-process lane. No Docker, shared deployment database/secret,
accept-all TLS verifier or mocked transport is acceptable. Reuse lifecycle ideas
from [the native grant-fence fixture](../../experiments/grant-fence/src/fixture.rs),
not its authorization experiment as proof of federation correctness.
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
   authenticated bytes retained across restarts. Assert the exact error table.
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
