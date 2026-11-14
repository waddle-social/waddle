# Issue 2047: proposed strict S2S verifier

## Status and relationship to the transport proposal

Amendment to the B2 gate in [the handshake design](s2s-handshake-2047.md),
stacked on PR #2074 at `bbf076dc4f9e5ef449ccd4cc0072b3b5682d74f9`. B2 is gate
G2 of [ADR-0020](../adrs/0020-independent-community-federation.md).

The verifier is implemented in `crates/waddle-server/src/s2s/verify/`. It is
a library only: it owns no socket, reads no file, is not used by the running
server and enables nothing. The decisions the verifier slice owns, for gate
G2 and for its part of gate G1, are recorded under
[Verifier decisions for G1 and G2](#verifier-decisions-for-g1-and-g2); where
an earlier section of this document differs from that section, that section
controls. Those decisions and the implementation need the independent review
that precedes the slice's merge into the integration branch. Nothing in this
document is executed evidence: under ADR-0020 D9 the evidence is the
verifier's focused set, the cases marked W4b in the
[matrix](#required-executable-negative-test-matrix), which runs when the
slice completes, and the verification phase. Parser validation for the
codec, the two-process transport evidence, and the identity,
grant/effect-fence and production activation gates in ADR-0020 are
unchanged.

**Amended by ADR-0020 decisions D1 and D2.** Where this document conflicts
with them, they control:

- A leaf whose extended key usage is serverAuth only is accepted in both TLS
  roles, in every trust profile and admission mode. This supersedes the
  explicit clientAuth usage that pipeline step 3 requires of the initiating
  peer's certificate: the initiating role accepts a leaf whose EKU includes
  serverAuth or clientAuth, and the receiving role still requires serverAuth.
  The stock client verifier's rejection of a serverAuth-only leaf in the probe
  table is library behavior, not policy. Let's Encrypt stopped issuing
  certificates with the clientAuth usage on 2026-07-08
  ([announcement](https://letsencrypt.org/2025/05/14/ending-tls-client-authentication/)).
  Accepting a serverAuth-only leaf from a TLS client departs from RFC 5280
  section 4.2.1.12; ADR-0020 records it as a deviation.
- The trust profile is chosen per domain, independently of the admission mode.
  Public WebPKI is the default for every domain. The per-peer private-CA
  profile described here applies to a domain only when the operator configures
  it for that domain; a public-CA chain then fails for that domain, and a
  private-CA chain fails for any domain on the public profile. The
  complete-CRL and fail-closed freshness rules below belong to the private-CA
  profile and are not applied as hard checks to public chains. The verifier
  slice selects and has reviewed the public profile's root store, its update
  policy and its revocation handling (gate G2). A profile is selected from
  configuration before the handshake, never as a fallback after another
  profile rejects a chain.
- Inbound streams use STARTTLS only, and the initial, pre-TLS stream header
  must carry `from`. The inbound reference domain is that domain, admitted
  under D1 before the handshake, so the profile and reference identity are
  known inside the TLS verifier callback in every mode; a peer that omits it
  is refused before TLS. Inbound direct TLS (XEP-0368) is deferred until a
  two-stage verification design is reviewed. On outbound streams, including
  direct TLS to `_xmpps-server` targets, the reference domain is the stream
  `to` domain as before.
- The public-WebPKI profile is tested in the verifier suite with a
  substituted test root set. Validation against the real public root store is
  evidence for the activation decision (gate G22), not for the native harness.
- Reference domains are compared in the canonical domain form that the
  identity-profile slice defines (gates G1 and G10), so a U-label domain and
  its A-label form are one reference identity. The verifier slice decides how
  that form maps onto DNS-ID and SRV-ID matching, which compare A-labels.

The remaining construction, limits and evidence requirements are unchanged,
and B2 remains open.

The executable fixture remains two native Waddle processes, loopback endpoints
and `.test` domains configured with the private-CA profile. Application
dispatch, grants, delivery custody, DNS discovery, public CA issuance and
production network exposure remain outside the verifier. The candidate
authenticates exactly the stream's admitted domain pair, never any identity
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
| `x509-parser` | `0.18.1` | Parse SAN `GeneralName::OtherName`, KU/EKU, validity, issuer and CRL metadata. Parsing alone authenticates nothing. Its `verify` feature is not enabled: startup signature checks go through webpki with the TLS provider's algorithms (see [algorithm intersection](#algorithm-intersection-g2)). |
| `webpki-roots` | `1.0.7` | `TLS_SERVER_ROOTS`: the Mozilla CA program's TLS server roots as `TrustAnchor` values, compiled in. The public-WebPKI profile's root store. |

Primary implementation references for those exact crate versions:
[rustls server verifier](https://docs.rs/crate/rustls/0.23.46/source/src/webpki/server_verifier.rs),
[rustls client verifier](https://docs.rs/crate/rustls/0.23.46/source/src/webpki/client_verifier.rs),
[webpki path verification](https://docs.rs/crate/rustls-webpki/0.103.16/source/src/verify_cert.rs),
[webpki CRL selection](https://docs.rs/crate/rustls-webpki/0.103.16/source/src/crl/mod.rs),
[webpki CRL parsing](https://docs.rs/crate/rustls-webpki/0.103.16/source/src/crl/types.rs),
[webpki name forms](https://docs.rs/crate/rustls-webpki/0.103.16/source/src/subject_name/mod.rs),
[x509-parser CRL API](https://docs.rs/crate/x509-parser/0.18.1/source/src/revocation_list.rs).
The source inspection used the matching locally cached crate sources.
`waddle-server` declares `rustls-webpki`, `webpki-roots` and `x509-parser` as
direct dependencies at the locked versions; no crate was added to the
lockfile. The verifier suite generates its PKI with `rcgen` 0.13.2, the
version the lockfile already holds as a development dependency, and signs
the CRL forms `rcgen` cannot produce with `p256`.

Stock `WebPkiClientVerifier` verifies clientAuth usage but takes no XMPP reference
domain. Stock `WebPkiServerVerifier` checks DNS/IP identities and serverAuth.
Both accept a missing EKU; normal webpki path verification ignores certificate
KeyUsage bits. Stock CRL verification defaults to ignoring expiration; an empty
CRL list disables revocation checks even when expiration enforcement is selected.
The candidate must therefore add policy checks, not just enable these builders.

## Candidate verification pipeline

One private, per-connection adapter owns the prepared reference domain, exact
pair authorization and immutable trust snapshot. It implements the two rustls
verifier traits with the same policy core. Select the reference domain's
configured profile before the handshake: the peer snapshot for a private-CA
domain, otherwise the public-WebPKI profile (ADR-0020 D2). No fallback to
another root set, peer-supplied endpoint name, HTTP metadata or
post-verification identity substitution is allowed.

1. Before verification, reject chains exceeding eight certificates or 64 KiB
   total DER, malformed DER/trailing data, excessive extensions or unsupported
   certificate forms. Parse bounded metadata once. Parsing does not produce an
   authenticated principal. The ceilings are listed under
   [Parser and CRL ceilings](#parser-and-crl-ceilings-g2).
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
   Under ADR-0020 D2, a serverAuth-only leaf satisfies the role check in both
   roles, as a recorded deviation from RFC 5280 section 4.2.1.12.
4. Match the authenticated leaf's XMPP reference identity as specified below.
   Produce the certificate-verification assertion only after chain, revocation,
   profile and identity checks all pass. A match in a failed chain must never
   cause fallback to success.
5. Retain rustls's TLS handshake signature verification. In the TLS 1.3 callback,
   delegate to `rustls::crypto::verify_tls13_signature` with the same provider
   algorithms and expose its supported schemes. Never assert
   `HandshakeSignatureValid` without verification. The
   [TLS profile](#tls-profile-g1) admits TLS 1.2, whose callback uses
   `verify_tls12_signature`. A verified certificate without a valid handshake
   signature does not authorize EXTERNAL.
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
SAN to reuse the DNS matcher. The DNS/SRV reference-list construction,
wildcard handling, IDNA rules and treatment of XmppAddr are decided under
[Reference identities and presented identifiers](#reference-identities-and-presented-identifiers-g1).
Tests must not claim broader identity support than that matrix.

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
it does not establish those properties. Verify the self-signature with the
root's own SPKI and check inner/outer signature-algorithm agreement. Root trust
still comes from operator provisioning, not from being self-signed. The
signature algorithms are exactly the rustls provider's; see
[Algorithm intersection](#algorithm-intersection-g2), which also records why
the check goes through webpki and not through `x509-parser`'s `verify` feature.

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
  agreement and cRLSign authorization, using the provider's algorithms through
  webpki ([Algorithm intersection](#algorithm-intersection-g2)). A metadata
  parser is not this proof.
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
ceilings are selected under
[Parser and CRL ceilings](#parser-and-crl-ceilings-g2); they are not yet
measured, and byte limits alone do not prove the 128-KiB handshake allocation
target.

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

## Verifier decisions for G1 and G2

The verifier slice (W4b) decided the following. Each heading names the gate
the decision belongs to. They are recorded here for independent review and
for the G4 review of the transport design; nothing in this section is
executed evidence. Their evidence is the verifier suite,
`crates/waddle-server/tests/s2s_verifier.rs`, which is the W4b focused set.

### Updates and errata disposition (G1)

The RFC Editor's errata listings were read on 2026-11-14 for the documents
that govern certificate verification and TLS on S2S streams.

| Document | Updates and errata | Disposition |
| --- | --- | --- |
| RFC 6120 | Updated by RFC 7590 and RFC 8553. Errata 4228 (reported; schema in appendix A.6), 4741 (held; IQ `id` in section 8.2.3) and 2855, 3486, 3650, 3651 (held, editorial). | None changes certificate or TLS behavior. 3486 (section 5.4.1: a server that *offers* TLS advertises STARTTLS) agrees with required STARTTLS. 3651 (section 4.9.3.19: the *initiating* entity decides whether to follow a redirect) is moot because Waddle never follows `see-other-host`. 4228 and 4741 concern stanzas and belong to S2S routing. RFC 8553 renames registry entries only: the SRV-ID service label stays `_xmpp-server`. |
| RFC 7590 | No errata. It incorporates BCP 195, which is now RFC 9325. | Followed through RFC 9325; see the TLS profile below. Its section 3.4 leaves the certificate rules of RFC 6120 in force. |
| RFC 9325 | No errata. | See the TLS profile below. |
| RFC 9525 | Obsoletes RFC 6125, which RFC 6120 cites. Erratum 8173 (held, editorial; example numbering in section 6.1.2). | RFC 9525's rules replace RFC 6125's wherever RFC 6120 defers to it: no CN-ID, a wildcard only as the complete left-most label, comparison of A-labels. RFC 6125's own errata are overtaken: 3090 (wildcards inside a label) by RFC 9525 section 6.3, and 5654 (the Server Name Indication carries the source domain) by ADR-0020 D3 and XEP-0368. |
| RFC 4985 | Erratum 2520 (verified, technical; section 2): the Name of an SRVName is a domain for which the subject is an authorized provider of the service. Other errata are editorial. | Applied. An SRV-ID names the stream's source domain, never an SRV target host. |
| RFC 5280 | Updated by RFC 6818 and later documents. Verified errata 5802 and 6414 (section 4.2.1.12: serverAuth and clientAuth are TLS usages, not web-only, and digitalSignature is consistent with them), 5938, 7661, 3579 and 7658; reported 9259 (section 6.1.5: an unprocessed critical extension in the target certificate fails validation); held 5997 (section 4.2.1.10: syntax of dNSName constraints) and others that are editorial or about issuance. | 5802 supports using the two usages for XMPP. 6414 is why digitalSignature is the key usage the profile requires. 9259 is already the behavior: an unknown critical extension in any certificate of the path is refused. For 5997 the verifier follows webpki's reading of dNSName constraints, including a leading dot meaning subdomains only. The internationalization update (RFC 9549) agrees with comparing dNSName entries as A-labels. Certificate-policy processing, which later updates revise, is not performed: neither profile uses policies. The remaining updates were not audited. |
| RFC 7622 | Owned by the identity profile. | See [the identity profile](identity-profile-2047.md). |

### TLS profile (G1)

- **Versions.** TLS 1.3 and TLS 1.2 are admitted and TLS 1.3 is preferred,
  as RFC 9325 section 3.1.1 requires. This supersedes the TLS 1.3-only
  restriction of the handshake fixture and of pipeline step 5. No earlier
  version can be negotiated: rustls implements none, and a ClientHello that
  offers at most TLS 1.1 is refused with a `protocol_version` alert before
  any certificate is read.
- **TLS 1.2.** Only the provider's suites are available: ECDHE key exchange
  with AEAD ciphers. rustls implements no RSA key transport, CBC suite,
  compression or renegotiation, which covers RFC 9325 sections 3.3, 3.5 and
  4. The extended master secret (RFC 7627) is required, not merely
  supported; a TLS 1.2 peer without it cannot connect. This is stricter than
  RFC 9325 section 3.5 and is a recorded restriction for the interoperability
  harness to confirm against the pinned Prosody and ejabberd.
- **Handshake signatures.** Both verifier traits delegate to
  `rustls::crypto::verify_tls13_signature` and
  `rustls::crypto::verify_tls12_signature` with the provider's algorithms. A
  verified chain without a valid handshake signature fails the handshake.
- **Resumption.** Off in both roles and both versions: no session store, no
  tickets, no TLS 1.2 session-ID resumption, no early data, no half-RTT
  data. RFC 9325 section 3.4 permits resumption; this profile forgoes it so
  that every connection verifies the chain, the CRLs and the name again.
  Certificate compression and secret extraction are off.
- **Server name and ALPN.** An initiating connection sends the stream's
  source domain as its Server Name Indication, on STARTTLS as well as on
  direct TLS, and offers the `xmpp-server` ALPN protocol on direct TLS only
  (XEP-0368). The verifier checks that the connection was opened for its
  reference identity and never takes an identity from the server name. A
  receiving connection uses neither value.
- **Client certificate.** A receiving connection requires one at the TLS
  layer, so a peer without a certificate fails the handshake. If the
  dialback design classifies an absent certificate as eligible for dialback
  (gate G5), that slice changes this, with its own review.
- **Per-connection configuration.** Every connection has its own rustls
  configuration and its own verifier, which refuses a second certificate.
  A configuration is built only for a private key that belongs to the
  certificate it presents.

### Reference identities and presented identifiers (G1)

The reference identity is the stream's source domain as a `CanonicalDomain`
of kind `Dns`: lowercase A-labels. A U-label spelling and its A-label
spelling are the same reference identity. A domain of any other kind (an IP
literal, a numeric IPv4 form, a single-label name) is refused when the
verifier is built. A certificate authenticates only that reference identity,
whatever other names it carries.

| Presented identifier | Decision |
| --- | --- |
| DNS-ID (`dNSName`) | Matched by webpki on the verified end-entity certificate. Labels are compared as A-labels without regard to ASCII case (RFC 9525 section 6.3). A trailing dot in a presented name does not match. |
| Wildcard | Accepted in a DNS-ID only, and only as the complete left-most label (RFC 6120 section 13.7.1.2.1, RFC 9525 section 6.3). It stands for exactly one label, so `*.example.org` matches `muc.example.org` and neither `example.org` nor `a.muc.example.org`. At least two labels must follow it. Any other use of `*` makes that entry invalid, and an invalid entry is ignored, not an error. |
| SRV-ID (`id-on-dnsSRV`, RFC 4985) | Accepted. The value must be exactly one IA5String inside the explicit tag, with no trailing DER. It is split at its first dot into a service label and a name. The service must be `_xmpp-server`, compared without regard to ASCII case; the name must equal the reference domain in A-label form. Nothing is stripped, so `_xmpp-server._tcp.<domain>` does not match, and neither `_xmpp-client` nor `_xmpps-server` is an S2S service (direct TLS is validated like STARTTLS, XEP-0368). No wildcard. |
| XmppAddr (`id-on-xmppAddr`, RFC 6120 section 13.7.1.4) | Accepted for backward compatibility, as RFC 6120 section 13.7.2.1 allows. The value must be exactly one UTF8String inside the explicit tag. It names a server only when it is a bare domain: a value with a localpart or resourcepart, or with a trailing dot, never matches. Its ASCII letters are lowercased and the result must be a canonical spelling of the reference domain (`CanonicalDomain::parse_exact`), in A-labels or U-labels; no other mapping is applied. No wildcard. |
| Common Name, URI, IP address | Never an identifier. |
| Order | DNS-ID, then SRV-ID, then XmppAddr. The evidence records which one matched. |
| Malformed or repeated XMPP identifier | An SRV-ID or XmppAddr entry that is not correctly encoded, an SRV-ID without a service label, an `otherName` whose type cannot be read, or the same SRV-ID or XmppAddr twice refuses the certificate, even when a DNS-ID matches. A well-formed XmppAddr that is not a bare domain, such as an account address, is not an error; it does not match. Other `otherName` types are ignored. |
| Name-constrained path | When the trust anchor or an intermediate of the verified path carries a nameConstraints extension, only DNS-IDs are matched. webpki applies dNSName constraints to dNSName entries, and nothing applies RFC 4985 SRVName constraints, so an issuer limited to one DNS subtree could otherwise name any domain through an SRV-ID or XmppAddr. |
| `otherName` constraint | A trust anchor or intermediate that constrains the `otherName` form is refused: at startup on a private-CA profile, in the handshake on the public profile. This holds even when a DNS-ID would match. |

Limits of this matrix: an internationalized name in a DNS-ID or SRV-ID is
matched in A-label form only, because both are IA5Strings; SRVName
constraints are not implemented, so a deployment whose CA relies on them
cannot use SRV-IDs; a certificate with only a DNS-derived name, such as an
SRV target host, does not authenticate the source domain.

### Certificate profile in both roles (G1, G2)

- The end-entity certificate must not be a CA certificate, must carry an
  extended key usage extension, and that extension must include serverAuth
  when the certificate is presented by the receiving server, or serverAuth
  or clientAuth when it is presented by the initiating server (ADR-0020 D2;
  the serverAuth-only case in the initiating role is the recorded deviation
  from RFC 5280 section 4.2.1.12). anyExtendedKeyUsage alone does not
  satisfy either role.
- A keyUsage extension, when present, must assert digitalSignature. The
  private-CA profile requires the extension; the public profile accepts its
  absence, which RFC 5280 section 4.2.1.3 defines as unrestricted.
- Every issuer in the verified path must assert the CA basic constraint and
  carry a keyUsage extension with keyCertSign, on both profiles. webpki
  checks the basic constraint and path length but ignores key usage. A CA
  certificate with an extended key usage extension must include a usage the
  role accepts; one without the extension is not restricted by it.
- An unknown critical extension in any certificate of the path is refused.
- These checks run on every candidate path through webpki's `verify_path`
  callback, so a path that breaks the issuer profile is refused and webpki
  looks for another; the path that is accepted has passed them.

### Trust profiles (G2)

- **Selection.** `TrustInventory` holds the public roots and, per domain,
  an optional private-CA profile. A verifier takes the profile the
  inventory holds for exactly its reference domain when it is built; a
  component domain is not covered by its parent's entry. One chain is
  verified against one profile.
- **Private-CA paths.** Paths are built from the profile's own validated
  intermediates. Intermediates a peer sends are read within the size
  ceilings and otherwise ignored, so a certificate issued under a CA the
  profile does not list has no path, and every issuer in a path has a CRL.
- **Validity window.** A private-CA profile is usable from the latest
  notBefore or thisUpdate of its CA certificates and CRLs until the earliest
  notAfter or nextUpdate among them. It is refused at startup outside that
  window and by every verification outside it. One expired CA certificate
  therefore ends the whole profile: an operator rotating a CA removes the
  old one before it expires. This is the snapshot rule of
  [Complete CRLs](#complete-crls-and-fail-closed-freshness) applied to CA
  certificates as well.
- **Expiry frontier.** `VerifiedPeer::expires_at` is the earliest expiry
  among the end-entity certificate and, on the public profile, the
  intermediates of its path, or, on the private-CA profile, the profile's
  window. The connection manager closes the stream no later, and checks
  `VerifiedPeer::is_current` before SASL EXTERNAL succeeds.
- **Public root store.** The public-WebPKI profile trusts
  `webpki_roots::TLS_SERVER_ROOTS`: the Mozilla CA program's roots for TLS
  server authentication, compiled into the binary. The alternatives were
  the host's trust store (`rustls-native-certs`), which differs between
  images and can be empty in a minimal container, and the platform verifier
  (`rustls-platform-verifier`), which cannot carry this profile's policy.
  A compiled-in set is the same in every deployment and changes only
  through a reviewed lockfile change.
- **Root store updates.** The set changes when `webpki-roots` is updated in
  `Cargo.lock` and a new build is deployed; nothing is fetched or reloaded
  at run time. Root removals reach a deployment only that way, so the crate
  is updated with routine dependency maintenance and without delay when
  Mozilla distrusts a CA. `webpki-roots` carries the name constraints
  Mozilla imposes on a root but not its distrust-after dates or other
  per-root policy, which are therefore not enforced.
- **Substituted roots.** `PublicRoots::from_certificates` replaces the set.
  The verifier suite uses it; validation against the real set is activation
  evidence (gate G22). Whether operator configuration may substitute roots
  is decided with the configuration format (gate G7).

### Revocation of public chains (G2)

Public chains are not checked for revocation. The verifier requests no OCSP
response, ignores a stapled one, fetches no CRL and does not enforce the
must-staple extension (RFC 7633). The reasons: a check that fails open adds
nothing against an attacker who can withhold the response, and RFC 9325
section 7.5 records that no complete solution exists; a check that fails
closed makes every handshake depend on a CA's responder within the
five-second handshake deadline; rustls offers no way to obtain a stapled
response for an initiating peer's certificate; and Let's Encrypt ended its
OCSP service in 2025.

What bounds the exposure instead:

- An end-entity certificate on the public profile is refused when its
  validity period exceeds 398 days (`MAX_PUBLIC_LEAF_VALIDITY_SECS`), the
  ceiling of the CA/Browser Forum Baseline Requirements since 2020, so the
  window is that of a public certificate and not arbitrary.
- No stream outlives its certificate or an issuer of its path, and every
  new connection verifies again, because nothing is resumed.
- The operator's denylist (ADR-0020 D1) refuses a domain whose key is known
  to be compromised, at stream open and at the protected-effect boundary.
  This is the strategy to distrust a revoked certificate that RFC 9325
  section 7.5 requires.
- A domain that needs revocation checking can be given a private-CA profile.

This is a gap against RFC 6120 section 13.7.2, which requires checking
CRLs or OCSP, and against section 13.7.2.3, which recommends periodic OCSP
queries on long-lived streams. It is a recorded limitation, not conformance,
and it is part of what the activation decision (gate G22) accepts.

### Algorithm intersection (G2)

One set of algorithms is used everywhere: the
`signature_verification_algorithms` of the TLS crypto provider, which is
ring in this server. Certificate chains and their CRLs are verified by
webpki with that set, handshake signatures by rustls with its mapping to
signature schemes, and the startup checks of a private-CA profile (the
anchors' self-signatures, each intermediate's signature under its issuer,
each CRL's signature) by webpki's `EndEntityCert::verify_signature` with the
same set. The startup checks therefore do not use `x509-parser`'s `verify`
feature, which has its own algorithm list. No algorithm is accepted at
startup that a handshake cannot verify, or the reverse.

With ring the set is ECDSA P-256 and P-384 with SHA-256 or SHA-384, Ed25519,
and RSA of 2048 to 8192 bits with PKCS #1 v1.5 or PSS and SHA-256, SHA-384
or SHA-512. SHA-1, RSA below 2048 bits and ECDSA P-521 are not accepted in
either profile. Inner and outer signature algorithms must agree in every
certificate and CRL.

### Parser and CRL ceilings (G2)

All ceilings are constants in `s2s::verify::limits`. Counts are taken by a
strict DER walk that allocates nothing, before a certificate or CRL reaches
`x509-parser` or webpki's owned CRL form.

| Ceiling | Value |
| --- | --- |
| Certificates in a presented chain, end-entity included | 8 |
| DER bytes of a presented chain | 64 KiB |
| DER bytes of one certificate, presented or configured | 16 KiB |
| Extensions in one certificate | 32 |
| Entries in one subjectAltName | 256 |
| Subtrees in one nameConstraints | 64 |
| CA certificates in a private-CA profile, anchors and intermediates | 8 |
| CRLs in a private-CA profile | 8 |
| DER bytes of one CRL | 1 MiB |
| Revoked-certificate entries in one CRL | 8,192 |
| Trust anchors in a substituted public root set | 512 |

Every presented certificate is read, one at a time, whether or not the path
uses it, and a certificate over a ceiling refuses the chain. `x509-parser`
allocates in proportion to the certificate it reads, a few hundred KiB at
most for a 16 KiB certificate, and that tree is dropped before the next
certificate is read; what the verifier keeps per certificate is a few
hundred bytes. A 64 KiB chain cannot arrive through rustls, whose handshake
messages are limited to 65,535 bytes, so that ceiling is a second bound.
webpki keeps every entry of a CRL in memory for the life of the profile,
roughly 150 bytes per entry by its data structure, about 1.2 MiB for a CRL
at the entry ceiling. These figures are an analysis of the code, not a
measurement; the 128 KiB handshake allocation target of the handshake design
is not shown to hold and stays open for the connection manager's resource
model (gate G7).

### Cancellation and resource bounds (G2)

- **Search bound.** A verification is one webpki path search over at most
  eight certificates. webpki ends a search after 100 signature checks,
  200,000 path-building steps or 250,000 name-constraint comparisons, and
  the verifier adds no loop of its own. The worst input the chain ceiling
  permits, seven certificates that are all candidate issuers of each other,
  ends at the signature budget. This is an algorithmic bound, not a
  wall-clock one; it is measured when the focused set runs.
- **Workers.** `VerifierWorkers` runs the part of a handshake that can
  verify a certificate off the async runtime. A connection hands each
  `process_new_packets` call to a worker and awaits its completion. At most
  four jobs execute at once (`VERIFIER_WORKERS`), each on its own
  short-lived thread, and at most sixteen callers wait for a worker
  (`VERIFIER_WAITERS`); a further request is refused immediately. No job is
  queued without a worker.
- **Cancellation.** A job holds its worker until it has finished executing.
  A connection that times out drops the completion and closes its socket;
  the late result is discarded, the job still runs to its end, and its
  worker is not available to anyone until then. Cancelling a wait gives up
  the place in line. A job that panics frees its worker and reports that it
  was lost. The connection manager supplies the deadlines and the
  per-connection generation that makes a late result harmless to it.
- **Not shown.** That four occupied workers leave unrelated WebSocket
  clients responsive, that a drain completes within five seconds while
  workers are occupied, and the memory a handshake uses in total, are
  connection-level evidence (steps 6 and 8).

### Carried to the connection manager (G7)

The verifier takes DER and a clock; everything around it belongs to the
connection manager (W4c):

- Reading the operator's files, decoding PEM and enforcing the file-size
  limits of the handshake design, then loading each `PrivateCaProfile` and
  building the `TrustInventory` before the `s2s` listener is acquired. A
  profile that fails to load stops S2S from starting.
- Per connection: a `PeerVerifier` for the stream's source domain, built
  after D1 admission from the pre-TLS `from` on a receiving stream or from
  the `to` domain on an initiating one; the matching `receiving_config` or
  `initiating_config`; and `VerifierWorkers` for every `process_new_packets`
  call of the handshake, with the connection's deadlines around it.
- After the handshake: `Verification::Verified` supplies the negotiation's
  `TlsPeer` and the certificate digest and expiry that `VerifiedPeerStream`
  records. `Verification::Failed` is an active certificate failure under
  ADR-0020 D2, after which no dialback is offered. The expiry is checked
  again before SASL EXTERNAL succeeds and closes the stream when it passes;
  a clock that cannot tell the time fails the connection.
- Telemetry. The verifier emits no metric. `VerifyError::code` and
  `TrustErrorKind` are bounded values for logs; a metric attribute still
  needs the telemetry allowlist.

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

The last row records stock library behavior. ADR-0020 D2 supersedes it as
policy: the candidate accepts a serverAuth-only leaf in both roles, so the
inbound path cannot apply the clientAuth usage that the stock client verifier
enforces.

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

Implement the following on the approved repository toolchain and dependency
lock. No skipped dependency counts as a pass. Each case names the slice that
owns it. The W4b cases are `crates/waddle-server/tests/s2s_verifier.rs`, run
from `server/` with
`cargo nextest run -p waddle-server --features test-support --locked --test s2s_verifier`:

- Cases marked W4b form the verifier slice's focused set (ADR-0020 D9). They
  run in-process against the verifier adapter, the startup trust inventory and
  in-memory rustls handshakes in both roles, need no S2S listener, second
  process, PostgreSQL or SpiceDB, run when the verifier slice completes, and
  must pass before the connection manager merges into the integration branch.
  They run again in verification step 2.
- Cases marked W4a or W4c need the codec or the connection manager. They are
  evidence for that slice and run in verification: the codec's suites in
  step 2, and the real native XEP-0178 two-process lane in steps 6 and 8. That
  lane uses separate persistence, secrets and SpiceDB and normal WSS
  authentication for local-progress assertions, as required by
  [server guidance](../../AGENTS.md).

| Boundary | Required cases and assertion | Owner |
| --- | --- | --- |
| Signed path and TLS ownership | Correct mutual TLS in both roles over an in-memory handshake; wrong CA, missing certificate, invalid certificate signature and forged CertificateVerify fail the handshake. | W4b |
| Signed path and EXTERNAL | A valid name with an invalid chain or signature never authenticates EXTERNAL on a real connection, in either direction. | W4c (step 6) |
| Roles/profile | Missing/wrong EKU, missing digitalSignature, CA-as-leaf, invalid issuer usages, unknown critical extensions, trailing DER and excessive certificate count/bytes fail at the defined stage. A serverAuth-only leaf authenticates in both roles. | W4b |
| TLS versions | A version or feature outside the G1 TLS profile is refused; every allowed version keeps handshake signature verification (`verify_tls13_signature`, and `verify_tls12_signature` if the profile admits TLS 1.2). | W4b |
| Trust profile | A public-CA chain for a domain configured with a private-CA profile fails; a private-CA chain for a domain on the public-WebPKI profile fails; no second profile is tried after a rejection. | W4b |
| Trust profile at the listener | An inbound initial header without a pre-TLS `from` is refused before TLS; no inbound direct-TLS listener exists. | W4a (step 2) for the header; W4c (step 6) for the listener |
| DNS/service identity | Wrong account/component domain, SAN alias outside the pair, CN-only leaf, SRV-only valid identity, wrong `_xmpp-server` service, inserted `_tcp`, malformed OtherName DER/IA5String and duplicate applicable entries. A certificate naming only a DNS-derived SRV target, not the source domain, fails. Reference identities in U-label and A-label form match the same names. Record positive exact-name matches and negative pair authorization separately. | W4b |
| Constraints | Enforced DNS constraints and explicitly rejected SRVName/OtherName-constrained CA/path; no retry that drops constraints or promotes an intermediate to a root. | W4b |
| Root/inventory | Expired/not-yet-valid/non-CA/bad-self-signature anchor, duplicate subject/different key, unknown intermediate, cycle, unsupported algorithm and invalid inventory path fail snapshot construction. | W4b |
| Root deadline on live streams | Root metadata expiry closes live streams. | W4c (step 6) |
| CRLs | Empty/missing bundle, unknown issuer, bad signature, missing/future/expired/reversed timestamps, missing chain coverage, revoked leaf/intermediate, duplicate clean/revoking CRLs in both orders, delta/indirect/IDP variants and unsupported critical extensions fail closed. Valid complete chain coverage passes. | W4b |
| Time | Certificate and CRL validity is evaluated at the supplied verification time, including not-yet-valid and expired material. | W4b |
| Time/rotation on live streams | Advance certificate and CRL deadlines during idle streams; roll back the wall clock; mutate snapshot files without restart; retire old processes, restart with new keys/CA/CRLs and reject old trust. No unsupported claim of instant remote revocation or anti-rollback. | W4c (steps 6 and 8) |
| Resource/cancellation in the verifier | Oversized DER/name/extension/CRL inventories; worst permitted path search; four occupied verifier workers plus new requests; cancellation during verification; late completion. Assert bounded allocations, no permit reuse before completion and finite worker release. | W4b |
| Resource/cancellation on connections | Slow socket and drain while verifier workers are occupied; local WSS progress within the fixture deadline. | W4c (steps 6 and 8) |
| Resumption in configuration | The configured client and server refuse tickets, early data and session resumption in an in-memory handshake, and every handshake reruns certificate, CRL and name checks. | W4b |
| Resumption across restarts | Fresh connections after restart rerun every check; EXTERNAL cannot reuse an old principal. | W4c (step 6) |

B2's design items are recorded under
[Verifier decisions for G1 and G2](#verifier-decisions-for-g1-and-g2) and are
independently reviewed before the verifier slice merges into the integration
branch. Its evidence records the exact implementation commit, library feature
set, commands, positive and negative results and time/memory measurements,
first when the W4b focused set runs at slice completion (ADR-0020 D9) and
again in verification; the implementation head is reviewed in the close-out
review. Still open after those decisions: their independent review; the
execution of the focused set; time and memory measurements of the ceilings,
the worst path search and the workers; validation of the public-WebPKI profile
against a real public peer (gate G22); and the connection-level cancellation,
drain and two-process authentication evidence, which belongs to the connection
manager. This document alone approves no transport implementation.

## Standards references

Use [RFC 6120](https://www.rfc-editor.org/rfc/rfc6120.html) for XMPP certificate
and negotiation rules, [RFC 4985](https://www.rfc-editor.org/rfc/rfc4985.html)
for SRVName encoding/constraints, [RFC 5280](https://www.rfc-editor.org/rfc/rfc5280.html)
for certificate/CRL profiles, and
[RFC 9525](https://www.rfc-editor.org/rfc/rfc9525.html) for current service identity.
[RFC 7590](https://www.rfc-editor.org/rfc/rfc7590.html) and
[RFC 9325](https://www.rfc-editor.org/rfc/rfc9325.html) govern the TLS profile.
The protocol sources are [XEP-0178 version 1.2](https://github.com/xsf/xeps/blob/4132c531487fe9450ae90b905b8df950d0d478b2/xep-0178.xml)
and [XEP-0368 version 1.1.0](https://github.com/xsf/xeps/blob/4132c531487fe9450ae90b905b8df950d0d478b2/xep-0368.xml)
at revision `4132c531487fe9450ae90b905b8df950d0d478b2`; the repository pins
them and the RFC texts under `xeps/`. The updates and errata disposition for
certificate verification and TLS is recorded under
[Updates and errata disposition](#updates-and-errata-disposition-g1); the
header, stream-error and SASL parts of B1 are recorded in the handshake
design. Stock-library acceptance is not a substitute for either.
