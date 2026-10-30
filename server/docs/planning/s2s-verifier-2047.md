# Issue 2047: proposed strict S2S verifier

## Status and relationship to the transport proposal

Proposed amendment to the open B2 gate in
[the handshake design](s2s-handshake-2047.md), stacked on PR #2074 at
`bbf076dc4f9e5ef449ccd4cc0072b3b5682d74f9`. This selects a candidate verifier
construction and records observed library behavior. It contains no runtime
implementation and does not complete B2. Independent architecture review,
standards-profile resolution, parser validation and executable transport evidence
remain required. The identity, grant/effect-fence and separate production
activation gates in [ADR-0020](../adrs/0020-independent-community-federation.md)
are unchanged.

Scope remains two native Waddle processes, loopback endpoints and `.test`
domains. Application dispatch, grants, delivery custody, DNS discovery, public
CA issuance and production network exposure remain outside this slice. The
candidate accepts the exact configured stream domain pair, never any identity
merely because it shares a certificate, parent domain or issuing CA.

## Locked libraries and actual API boundaries

The inspected [server lockfile](../../Cargo.lock) contains these versions. Direct
workspace dependencies would need to be declared in a future implementation;
transitive availability is not an API ownership decision.

| Library | Version | Selected API or limitation |
| --- | --- | --- |
| `rustls` | `0.23.46` | `client::danger::ServerCertVerifier`, `server::danger::ClientCertVerifier`; `crypto::verify_tls13_signature` preserves TLS proof of private-key possession. |
| `rustls-webpki` | `0.103.16` | `EndEntityCert::verify_for_usage`, `VerifiedPath`, `RevocationOptionsBuilder`; separate `verify_is_valid_for_subject_name` supports DNS/IP, not SRV-ID. |
| `rustls-pki-types` | `1.14.1` | Typed DER, `TrustAnchor`, `ServerName`, `UnixTime`; an extracted trust anchor does not retain all certificate policy metadata. |
| `tokio-rustls` | `0.26.4` | Transport adapter; using it does not make synchronous verification cancellable or establish a total memory cap. |
| `x509-parser` | `0.18.1` | Parse SAN `GeneralName::OtherName`, KU/EKU, validity, issuer and CRL metadata. Parsing alone authenticates nothing. The future startup signature checks require its explicit `verify` feature, which uses ring. |

Primary implementation references for those exact crate versions:
[rustls server verifier](https://docs.rs/crate/rustls/0.23.46/source/src/webpki/server_verifier.rs),
[rustls client verifier](https://docs.rs/crate/rustls/0.23.46/source/src/webpki/client_verifier.rs),
[webpki path verification](https://docs.rs/crate/rustls-webpki/0.103.16/source/src/verify_cert.rs),
[webpki CRL selection](https://docs.rs/crate/rustls-webpki/0.103.16/source/src/crl/mod.rs),
[webpki CRL parsing](https://docs.rs/crate/rustls-webpki/0.103.16/source/src/crl/types.rs),
[webpki name forms](https://docs.rs/crate/rustls-webpki/0.103.16/source/src/subject_name/mod.rs),
[x509-parser CRL API](https://docs.rs/crate/x509-parser/0.18.1/source/src/revocation_list.rs).
The source inspection used the matching locally cached crate sources.

Stock `WebPkiClientVerifier` verifies clientAuth usage but takes no XMPP reference
domain. Stock `WebPkiServerVerifier` checks DNS/IP identities and serverAuth.
Both accept a missing EKU; normal webpki path verification ignores certificate
KeyUsage bits. Stock CRL verification defaults to ignoring expiration; an empty
CRL list disables revocation checks even when expiration enforcement is selected.
The candidate must therefore add policy checks, not just enable these builders.

## Candidate verification pipeline

One private, per-connection adapter owns the prepared reference domain, exact
pair authorization and immutable trust snapshot. It implements the two rustls
verifier traits with the same policy core. Select the peer snapshot from the
already configured pair; no global-root fallback, peer-supplied endpoint name,
HTTP metadata or post-verification identity substitution is allowed.

1. Before verification, reject chains exceeding eight certificates or 64 KiB
   total DER, malformed DER/trailing data, excessive extensions or unsupported
   certificate forms. Parse bounded metadata once. Parsing does not produce an
   authenticated principal. The concrete parser allocation budget is still open.
2. Call `EndEntityCert::verify_for_usage` with the chosen peer's anchors,
   configured provider signature algorithms, peer intermediates, current time,
   role-specific EKU and explicit full-chain revocation options. Do not use
   `verify_server_cert_signed_by_trust_anchor` as a shortcut: that rustls helper
   performs no revocation checking. Do not substitute parsed names for a valid
   signed path.
3. Use the returned `VerifiedPath` and, where needed, its `verify_path` callback
   to enforce the profile on the actual selected leaf/intermediates/anchor.
   Check the leaf's required digitalSignature KU, explicit serverAuth/clientAuth
   roles and non-CA status. Require explicit CA/keyCertSign and applicable
   cRLSign usages for issuers. The fixture issues dual-role leaf certificates;
   this is a restricted issuance policy, not a claim that XMPP universally
   requires both EKUs. Every candidate path must satisfy the same policy.
4. Match the authenticated leaf's XMPP reference identity as specified below.
   Produce the certificate-verification assertion only after chain, revocation,
   profile and identity checks all pass. A match in a failed chain must never
   cause fallback to success.
5. Retain rustls's TLS handshake signature verification. In the TLS 1.3 callback,
   delegate to `rustls::crypto::verify_tls13_signature` with the same provider
   algorithms and expose its supported schemes. Never assert
   `HandshakeSignatureValid` without verification. If the B1 profile later admits
   TLS 1.2, its callback must use `verify_tls12_signature`; until then the config
   does not enable TLS 1.2. A verified certificate without CertificateVerify
   success does not authorize EXTERNAL.
6. Only after the TLS handshake completes may the transport use its evidence
   for the EXTERNAL and post-SASL transitions already defined in #2074. Recheck
   the snapshot's time validity before EXTERNAL success. Retain the actual
   selected certificate/CRL expiry frontier, not merely the leaf's expiry.

Typed errors must preserve which check failed internally while exposing only the
bounded failure contract from #2074. Telemetry must not contain certificate
bodies, private keys, raw peer stanzas or identities as metric labels.

## DNS-ID, SRV-ID and constraints

Outbound reference domain is the configured stream destination. Inbound
reference domain is the prepared stream source bound to an allowed pair. The
socket endpoint IP and TLS SNI are not alternative reference identities.

Use webpki's DNS matcher on the unmodified authenticated leaf for DNS-ID.
Independently parse SRV-ID SAN OtherName OID `1.3.6.1.5.5.7.8.7`, requiring exactly
one correctly wrapped IA5String with no trailing DER. The service identity is
`_xmpp-server.home-a.test`, without a `_tcp` label. Decode to a dedicated typed
service/domain value; compare the service and prepared domain under the approved
RFC matrix. Never compare a substring or strip an arbitrary service prefix.
No Common Name or URI fallback is permitted. Duplicate/malformed applicable name
entries fail closed rather than allowing a later match to hide an invalid field.

The initial fixture issues exact DNS names and can include exact SRV names.
Supporting SRV-ID-only certificates requires the adapter matcher: the stock
webpki subject-name API supports only DNS/IP. Name matching must operate on the
original leaf verified by webpki; do not synthesize a certificate or replace its
SAN to reuse the DNS matcher. The precise DNS/SRV reference-list construction,
wildcard handling, IDNA rules and treatment of optional XmppAddr remain B1
questions. Tests must not claim broader identity support until those rules and
the adapter are reviewed and executed.

Preserve webpki DNS/IP name-constraint enforcement on the chosen chain. webpki
represents OtherName as unsupported and rejects applicable OtherName constraints;
it does not implement RFC 4985's SRVName-constraint matching. For this candidate,
fail closed on any SRVName/OtherName name constraints in a configured CA or peer
path; do not intercept `NameConstraintViolation` and retry without constraints.
This restriction also applies if a DNS-ID would otherwise match. Supporting
constrained SRVName chains needs its own reviewed implementation. It is an
explicit interoperability limitation, not evidence that all XMPP identity
requirements are met. The fixture's CAs must conform to the restricted profile.

Even a successful DNS/SRV match only authenticates the exact configured pair.
A wildcard or multi-SAN certificate never grants another service domain. Host
policy remains separate from certificate identity.

## Trust-anchor and issuer inventory

Extend the proposed peer snapshot with a bounded public CA-certificate inventory.
Only entries explicitly marked as trust anchors may become `TrustAnchor`s;
intermediates in that inventory never become roots through discovery. Retain
original DER, subject/SPKI fingerprints, constraints and validity metadata for
each configured CA. Cap the inventory at eight CA certificates per peer and
include it in #2074's total material limit.

At startup, validate each configured root's DER, self-signature, current validity,
explicit CA basic constraint and the fixture's key usages/extension policy before
extracting a webpki anchor. `anchor_from_trusted_cert` assumes prevalidated input;
it does not establish those properties. Use `x509-parser`'s feature-gated
`verify_signature` API with the root's own SPKI for the self-signature, and check
inner/outer signature-algorithm agreement. Root trust still comes from operator
provisioning, not from being self-signed. The accepted signature algorithms must
be constrained consistently with the rustls provider; exact intersection is an
open B2 proof item.

Validate provisioned intermediate CA paths to explicit anchors, including
signatures, constraints, usages, current validity and revocation; validate their
CRLs before permitting S2S binds. For the first fixture use a single unambiguous
issuer path per CA. Unknown issuer, cycles, duplicate subject identities or an
unverifiable inventory fail startup. Do not promote peer-supplied intermediate
certificates to trusted configuration. Peer paths must use the configured issuer
inventory so required CRL signature/coverage checks can be completed at startup.

Require unique issuer subject DER within a peer snapshot, including during CA
rotation. This intentionally rejects same-subject/different-key overlap rather
than allowing webpki's first-applicable-CRL selection to depend on file order.
Planned overlap may use separately identified CAs with distinct subjects; removal
still requires retiring every old-generation process. A different rotation
profile needs a reviewed issuer-selection contract.

## Complete CRLs and fail-closed freshness

Select complete, direct CRLs only. Reject delta CRLs, indirect CRLs and issuing-
distribution-point variants in this first profile. For each configured issuer,
require exactly one CRL covering its issued certificates; reject duplicate CRLs,
even byte-identical duplicates, rather than silently choosing one. Missing or
empty bundles fail startup. Reject unknown CRL issuers and unsupported critical
extensions. Cap the bundle at eight CRLs per peer; existing per-file and total
material limits still apply.

At startup, parse each entire DER value and establish:

- Its issuer maps uniquely to the provisioned CA, with compatible AKI/key data.
- Its signature verifies with that issuer's SPKI, with inner/outer algorithm
  agreement and cRLSign authorization; use `x509-parser::CertificateRevocationList::verify_signature`
  with the explicit `verify` feature. A metadata parser is not this proof.
- `thisUpdate <= now < nextUpdate`, with both timestamps required and ordered.
  Future, missing or expired metadata fails closed. Record `nextUpdate` for the
  live-stream deadline. A new UUID does not make an old CRL newer or trustworthy.

Feed the same validated CRLs into webpki; it revalidates signatures and checks
revoked serials while building the actual chain. Construct
`RevocationOptionsBuilder::new(nonempty_refs)` with
`RevocationCheckDepth::Chain`, `UnknownStatusPolicy::Deny` and
`ExpirationPolicy::Enforce`. Do not call `only_check_end_entity_revocation` or
`allow_unknown_revocation_status`. Do not swallow an empty-builder error by
turning revocation into `None`.

The prechecks are necessary: the pinned webpki parser discards `thisUpdate`, and
its verifier uses the first authoritative CRL. Our observed duplicate-order case
must fail at startup in both orders. Recheck time before each verification and
EXTERNAL completion; timers close existing streams at the earliest snapshot/path
expiration as required by #2074.

There is no network CRL/OCSP retrieval or silent recovery from an unavailable
revocation authority. Files are immutable process snapshots. Updating a file
does not update running trust. Drain/exit every old process before declaring its
generation retired, then restart with validated material. Newly published
revocations are not instantaneous: restart or the freshness deadline bounds their
adoption. This profile has no durable anti-rollback ledger for operator-provided
CRLs; it cannot prove the supplied still-current CRL is the issuer's latest. That
limitation must remain explicit and blocks any stronger production revocation
claim. Production freshness and rollback policy require separate review.

## Resource, cancellation and lifecycle limitations

Retain #2074's admission, file, certificate, connection and queue limits. Require
bounded extension/name/CRL-entry counts before constructing owned metadata. The
exact parser-allocation and CRL-entry ceilings still need to be selected and
measured: byte limits alone do not prove the 128-KiB handshake allocation target.

Pinned rustls internally limits handshake messages/deframer buffering to 65,535
bytes and limits certificate-list encoding. `ConnectionCommon::set_buffer_limit`
controls outgoing plaintext/TLS buffers; it is not a cap on all receive,
certificate or verifier allocations. Do not document it as one. Explicitly clear
certificate compression/decompression lists so compressed certificate paths do
not add an unreviewed expansion boundary.

webpki has internal per-path-search budgets of 100 signature checks, 200,000
chain-building calls and 250,000 name-constraint comparisons. Those are finite
algorithmic bounds, not a five-second wall-clock proof. The stock verifier traits
are synchronous. Tokio timeout/cancellation cannot stop verification already
executing, including a `spawn_blocking` job.

The candidate uses a dedicated bounded worker budget for expensive TLS processing,
acquired before enqueueing work. The worker owns its permit until execution ends,
even if the connection has timed out; cancellation may discard a completion but
cannot return that permit early. No unbounded worker queue or replacement jobs.
Connection generation checks reject late results. A timeout closes the socket;
it does not prove the worker stopped or its memory was released. The design must
prove local progress and finite worker release under the worst permitted input,
or choose process isolation/stronger algorithm limits before B2 closes. The
five-second process drain and per-connection memory guarantees remain open until
those measurements and shutdown behavior are demonstrated.

Use fresh rustls configs per immutable snapshot. Set client
`resumption = Resumption::disabled()` and `enable_early_data = false`; set server
`session_storage = Arc::new(NoServerSessionStorage {})`, `send_tls13_tickets = 0`
and `max_early_data_size = 0`, and do not configure a ticket producer. Keep
certificate compressor/decompressor lists empty. No TLS cache survives restart.
These public APIs provide configuration control, not live proof that resumption
or an old-generation connection cannot occur.

## Standalone probe evidence and its limits

On 2026-10-30, a local `/tmp/waddle-s2s-b2-probe` program invoked the stock verifier
APIs with in-memory test certificates and CRLs. Command:

```sh
cargo +1.98.0 run --offline --locked --manifest-path /tmp/waddle-s2s-b2-probe/Cargo.toml
```

All 13 assertions completed successfully. An assertion succeeding means the
observed accept/reject result matched the listed stock behavior, including
behaviors the stricter candidate must reject. The probe did not implement the
candidate adapter, establish a TCP/TLS handshake, exercise CertificateVerify,
run two servers, measure resource limits or complete B2.

| Case | Observed stock result |
| --- | --- |
| DNS SAN, dual EKU and current CRL | Accepted |
| Empty CRL bundle despite expiration enforcement | Accepted |
| Future `thisUpdate` | Accepted |
| Expired `nextUpdate`, expiration enforced | Rejected |
| Revoked leaf | Rejected |
| Clean CRL before an applicable revoking CRL | Accepted |
| Applicable revoking CRL before clean CRL | Rejected |
| Missing EKU | Accepted |
| Key-encipherment KU without digitalSignature | Accepted |
| SRV-ID-only certificate through standard DNS verifier | Rejected |
| Expired CRL with default expiration policy | Accepted |
| Dual EKU through client verifier | Accepted |
| ServerAuth-only EKU through client verifier | Rejected |

The SRV probe also parsed the expected OtherName OID and wrapped IA5String; that
was an observation, not another assertion or a verified SRV matcher. Runtime was
Rust **1.98.0**, not repository toolchain **1.98.1**. The probe pinned rustls
0.23.46, webpki 0.103.16, pki-types 1.14.1 and x509-parser 0.18.1, used rcgen
0.14.10 for fixtures, and independently resolved its other transitive dependencies.
It is not a repository-lockfile or CI result. No keys were exported.

Local evidence SHA-256 values, useful only with the retained `/tmp` files:

| File | SHA-256 |
| --- | --- |
| `src/main.rs` | `83bdad726312921a4affd59f2f122af2b6204434ce235d2139aab70fab49f8f0` |
| `Cargo.toml` | `043547eeaad680b29704963fb851d4bef922a00dab57659544a0b49132bd50a4` |
| `Cargo.lock` | `0100878a7eb7c3314d3197dd984011157cc17e5abe1650acc9b5740f84e8b2d5` |
| `results.txt` | `8dc8bb83ce2d7a2426fddb55215ecc346d3db2b461ad68b35d970439d351210b` |

These files are not committed by this design change. Their hashes are not durable
executable acceptance evidence; future repository tests must reproduce the cases.

## Required executable negative-test matrix

Implement the following in dedicated verifier tests and the real native
XEP-0178/two-process lane on the approved repository toolchain and dependency
lock. Use separate persistence/secrets/SpiceDB and normal WSS authentication for
local-progress assertions, as required by [server guidance](../../AGENTS.md).
No skipped dependency counts as a pass.

| Boundary | Required cases and assertion |
| --- | --- |
| Signed path and TLS ownership | Correct mTLS both directions; wrong CA, missing certificate, invalid certificate signature and forged CertificateVerify must fail. A valid name with invalid chain/signature must never authenticate EXTERNAL. |
| Roles/profile | Missing/wrong EKU, missing digitalSignature, CA-as-leaf, invalid issuer usages, unknown critical extensions, trailing DER and excessive certificate count/bytes fail at the defined stage. |
| DNS/service identity | Wrong account/component domain, SAN alias outside the pair, CN-only leaf, SRV-only valid identity, wrong `_xmpp-server` service, inserted `_tcp`, malformed OtherName DER/IA5String and duplicate applicable entries. Record positive exact-name matches and negative pair authorization separately. |
| Constraints | Enforced DNS constraints and explicitly rejected SRVName/OtherName-constrained CA/path; no retry that drops constraints or promotes an intermediate to a root. |
| Root/inventory | Expired/not-yet-valid/non-CA/bad-self-signature anchor, duplicate subject/different key, unknown intermediate, cycle, unsupported algorithm and invalid inventory path fail startup; root metadata deadline closes live streams. |
| CRLs | Empty/missing bundle, unknown issuer, bad signature, missing/future/expired/reversed timestamps, missing chain coverage, revoked leaf/intermediate, duplicate clean/revoking CRLs in both orders, delta/indirect/IDP variants and unsupported critical extensions fail closed. Valid complete chain coverage passes. |
| Time/rotation | Advance certificate and CRL deadlines during idle streams; rollback wall clock; mutate snapshot files without restart; retire old processes, restart with new keys/CA/CRLs and reject old trust. No unsupported claim of instant remote revocation or anti-rollback. |
| Resource/cancellation | Oversized DER/name/extension/CRL inventories; worst permitted path search; four occupied verifier workers plus new connections; cancellation during verification; late completion; slow socket and drain. Assert bounded allocations, no permit reuse before completion, finite worker release and local WSS progress within the fixture deadline. |
| Resumption | Attempt tickets, early data and fresh connections after restart; assert certificate/CRL/name/pair checks actually rerun and EXTERNAL cannot reuse an old principal. |

Before closing B2, record the exact implementation commit, library feature set,
commands, positive and negative results, time/memory measurements and independent
review at that head. Until then, unresolved items are: B1's reference-identity
matrix; the adapter and startup inventory/CRL construction; cryptographic algorithm
intersection; parser/CRL-count ceilings; cancellation/drain/resource proofs; and
real two-process authentication evidence. No transport implementation is approved
by this document alone.

## Standards references

Use [RFC 6120](https://www.rfc-editor.org/rfc/rfc6120.html) for XMPP certificate
and negotiation rules, [RFC 4985](https://www.rfc-editor.org/rfc/rfc4985.html)
for SRVName encoding/constraints, [RFC 5280](https://www.rfc-editor.org/rfc/rfc5280.html)
for certificate/CRL profiles, and
[RFC 9525](https://www.rfc-editor.org/rfc/rfc9525.html) for current service identity.
The protocol source remains [XEP-0178 version 1.2](https://github.com/xsf/xeps/blob/4132c531487fe9450ae90b905b8df950d0d478b2/xep-0178.xml)
at revision `4132c531487fe9450ae90b905b8df950d0d478b2`. The parent proposal's B1
updates/errata and precedence review remains open; stock-library acceptance is
not a substitute for that review.
