# Issue 2047: S2S Server Dialback profile, certificate outcomes and residual risk

## Status and scope

Decisions of the discovery and dialback slice (W4d) of
[the delivery plan](../../../docs/planning/federation-2047.md) for gate G5
of [ADR-0020](../adrs/0020-independent-community-federation.md): the
XEP-0220 wire profile Waddle speaks in each role, the classification of
certificate outcomes that decides when dialback may follow, the XEP-0185
key and its secret, the `dialback` authentication strength, and the
residual risk of keys that are not bound to the TLS channel. ADR-0020 D2
fixes the policy; this document records how it is implemented and the
choices D2 left to this slice.

Gate G5 must be reviewed before W4d merges and again before inbound
dialback is enabled on any deployment. Every dialback role is off by
default and nothing in this slice enables one.

Nothing here is executed evidence. The evidence is
`crates/waddle-server/tests/xep0220_s2s_dialback.rs` (XEP-0185 vectors,
the codec in every role, configuration, and in-process deployments on
loopback), compiled and run in the verification phase (steps 2, 6 and 7),
and the interoperability harness (W10) against the pinned Prosody and
ejabberd.

The implementation is `crates/waddle-xmpp/src/s2s/negotiation/` (the
dialback states of the initiating and receiving machines, `dialback.rs` for
the elements, and verification connections),
`crates/waddle-server/src/s2s/dialback.rs` (keys),
`crates/waddle-server/src/s2s/conn/verification.rs` (verification
connections), the dialback paths of `conn/session.rs` and the
`[dialback]` table of the configuration.

Sources: XEP-0220 1.1.1 and XEP-0185 1.0 at xsf/xeps
`4132c531487fe9450ae90b905b8df950d0d478b2`, RFC 6120 and RFC 7590.

## Configuration and roles

```toml
[dialback]
outbound = false     # Initiating and Authoritative Server roles
inbound = false      # Receiving Server role for admission.dialback
secret_file = "/absolute/path"  # required when outbound is on
```

- **Initiating Server** (`outbound`): an outbound stream whose receiving
  server, after TLS, does not offer SASL EXTERNAL authenticates this
  deployment by dialback instead of failing. Waddle has already verified
  the receiving server's certificate for the remote domain on that
  connection, as on every outbound connection; dialback never replaces
  Waddle's own PKIX check of a server it connects to.
- **Authoritative Server** (`outbound`): an inbound stream answers
  `<db:verify/>` requests about keys this deployment issued. It needs the
  same secret, so both roles share the one switch.
- **Receiving Server** (`inbound`): an inbound stream from a domain on
  `admission.dialback` that D1 also admits may authenticate by dialback.
  Waddle verifies the key with the Authoritative Server of the claimed
  domain over a verification connection (below).
- `admission.dialback` is separate from the allowlist and never admits a
  domain; the denylist overrides it, and configuration that lists a
  denylisted domain fails to load (connection-manager design). A dialback
  list while `inbound` is off, `inbound` with an empty list, or a secret
  while `outbound` is off loads with a warning that names the setting.
- **Secret.** The file's bytes, with one trailing `\n` or `\r\n` removed,
  at least 32 bytes, read through the bounded material reader and wiped
  after the HMAC key is derived. Every replica of one deployment that can
  answer verification requests holds the same secret, so a key one replica
  issued verifies on any other with no shared state; no other deployment
  holds it. Missing or short secrets fail the load
  (`DialbackSecretMissing`, `DialbackSecretTooShort`).
- **Rotation** is a restart of every replica with the new file. Keys
  issued under the old secret stop verifying at once, so dialback streams
  being negotiated during the rollout fail and are retried; established
  streams are unaffected. Overlapping secrets are not supported.

## Keys (XEP-0185)

`key = HMAC-SHA256(hex(SHA-256(secret)), receiving ' ' originating ' ' id)`
in lowercase hexadecimal, as XEP-0185 §3 recommends and its and XEP-0220's
examples compute. Both domains are the identity profile's canonical form
(lowercase A-labels, no trailing dot), so a peer that echoes them in a
U-label or another accepted spelling reaches the same key. `id` is the ID
of the response stream header the Receiving Server sent on the stream the
key travels on. Verification recomputes the key from the request's
attributes and compares in constant time; hexadecimal case is ignored. A
key is between one and 1,024 visible ASCII characters (the codec refuses
anything else as `bad-format`) and is never logged; `DialbackKey`'s debug
form is redacted.

Verification is stateless by design: the Authoritative Server does not
check that it issued a key for a stream it remembers, because the stream
may have been opened by another replica. The HMAC binds the key to the
pair and the stream ID the Receiving Server chose.

## Wire profile

Common rules:

- **TLS always.** No dialback element is accepted or sent before TLS. An
  inbound stream is STARTTLS only; an outbound stream or verification
  connection uses STARTTLS or XEP-0368 direct TLS as its endpoint says.
  Waddle never answers a verification request over a plaintext connection,
  so a peer that verifies keys without TLS, which XEP-0220 permits, cannot
  verify Waddle.
- **One pair per stream.** A stream carries the pair its header named; a
  dialback element naming another sender or target domain is
  multiplexing, which Waddle neither accepts nor initiates (ADR-0020
  transport decision), and ends the stream with `invalid-from` (another
  sender) or `host-unknown` (another target), the conditions a stanza for
  that pair gets. A missing or unparseable `from` or `to` is
  `improper-addressing`.
- **Unsolicited and uncorrelated elements** (XEP-0220 §4.1) end the stream
  with `policy-violation`: any `type` on an element received inbound (an
  inbound stream carries only requests), a `<db:result/>` result on an
  outbound stream that sent no key or for another pair, a `<db:verify/>`
  answer on a stream that asked nothing, or for another pair or stream ID,
  and any other element in the dialback namespace.
- **Advertisement.** A header declares `xmlns:db='jabber:server:dialback'`
  wherever a dialback role could apply to the stream: initiating headers
  when `outbound` is on, every verification-connection header, and
  response headers when `outbound` is on or the pair may use inbound
  dialback. The `<dialback xmlns='urn:xmpp:features:dialback'><errors/>
  </dialback>` feature is offered after TLS exactly where the pair may use
  inbound dialback.
- Emitted `from` and `to` are U-labels, as on stream headers; received ones
  are read with the stream-header address reader into the canonical form.

Receiving role (inbound):

- After the post-TLS header, a certificate verified for the claimed domain
  gets SASL EXTERNAL, with the dialback feature beside it if the pair may
  use dialback. Without any certificate (see the classification below),
  the stream gets the dialback feature alone if the pair may use dialback,
  empty features if only `outbound` is on (so the peer can only ask
  verification requests), and otherwise `policy-violation`.
- **Receiving Server.** At most one `<db:result/>` request per stream,
  accepted only after the post-TLS features, where the pair may use
  dialback and the key is present. The connection manager then checks that
  the domain is still admitted, still listed and that the registry could
  take the stream, and verifies the key. A valid key is answered
  `<db:result type='valid'/>` and the stream is established at once with
  the `dialback` strength: there is no restart, so the decoder leaves its
  pre-authentication mode there and the stream limits apply from the next
  element. Admission and registry capacity are checked again when the
  answer arrives. An invalid key is answered `type='invalid'`; a key that
  could not be verified is answered `type='error'` with the condition
  below. Either way the stream then closes without a stream error, because
  it carries no other pair. XEP-0220 §2.5 calls a dialback error non-fatal
  for the stream; closing it is a restriction of this profile.
- **Authoritative Server.** `<db:verify/>` requests are answered after the
  post-TLS features in every later state, including on an established
  stream, from the stream's source about a key issued to it by the
  stream's destination: `from` and `to` swapped, `id` copied, `type`
  `valid` or `invalid`. At most eight per stream, then `policy-violation`,
  so one connection cannot use the server as an unbounded key oracle.

Initiating role (outbound):

- After TLS has verified the receiving server, post-TLS features that
  offer SASL EXTERNAL are always used; dialback is used only when they do
  not and `outbound` is on. Waddle then sends `<db:result/>` with the key
  for the remote domain, itself and the stream ID of the receiving
  server's post-TLS header, and accepts only the typed `<db:result/>` for
  that pair. `valid` establishes the stream with the `dialback` strength;
  `invalid` or `error` closes it, which backs the domain off like any
  peer failure.

Verification connection (Receiving Server to Authoritative Server):

- A separate connection per inbound key, found like any outbound
  destination (configured endpoints or discovery, with the address
  checks), STARTTLS or direct TLS, with the Authoritative Server's
  certificate verified for the claimed domain under that domain's trust
  profile. Its header names this deployment as `from`, so this profile's
  rule that inbound streams carry a pre-TLS `from` holds for Waddle's own
  verification connections. It never authenticates and never carries
  stanzas: after the post-TLS features it sends one `<db:verify/>`
  (`from` this deployment, `to` the claimed domain, `id` the inbound
  stream's ID), accepts only the answer correlated to it and closes.
- It takes an outbound socket without evicting any stream, a token of the
  global dial rate and an outbound handshake slot, and ends by the inbound
  stream's dialback deadline (`limits.dialback_timeout_ms`, 15 s, within
  the 20 s negotiation deadline). One inbound stream causes at most one
  verification connection, and inbound streams without a verified
  certificate stay within the unauthenticated socket caps (8 globally,
  2 per source address), which bounds how many a remote party can cause.
- Outcomes for the inbound peer:

  | Verification connection | Answer to the Initiating Server |
  | --- | --- |
  | `<db:verify type='valid'/>` | `type='valid'` |
  | `<db:verify type='invalid'/>` | `type='invalid'` |
  | `<db:verify type='error'/>` | `type='error'`, `remote-server-not-found` |
  | No endpoint, every address blocked, or not a DNS domain | `type='error'`, `remote-server-not-found` |
  | DNS lookup failed, TCP connect or TLS failed | `type='error'`, `remote-connection-failed` |
  | Authoritative Server's certificate failed | `type='error'`, `not-acceptable` |
  | Dial rate, socket or handshake cap | `type='error'`, `resource-constraint` |
  | Deadline, closed without an answer, or any other failure | `type='error'`, `remote-server-timeout` |

## Classification of certificate outcomes

ADR-0020 D2: an active certificate failure is a presented certificate
that fails validation for the claimed domain under that domain's trust
profile, whether presented by the inbound peer or by the Authoritative
Server; it is never followed by dialback. The remaining outcomes are
classified here.

| Where | Outcome | Classification | What follows |
| --- | --- | --- | --- |
| Inbound peer | Certificate verified for the claimed domain | Verified | SASL EXTERNAL; dialback beside it only for a listed pair, which then authenticates as `dialback` |
| Inbound peer | Certificate presented and refused: chain, trust profile, identity, validity, revocation (private-CA CRLs), key usage or handshake signature | Active failure | The TLS handshake fails before any XML; no dialback, no SASL |
| Inbound peer | No certificate, where the connection allows one to be absent | Absent | Dialback for a listed pair; otherwise only verification requests, or `policy-violation` |
| Inbound peer | No certificate, where the connection requires one | — | The TLS handshake fails |
| Authoritative Server | Certificate verified for the claimed domain | Verified | The verification request is sent |
| Authoritative Server | Certificate refused, or none | Active failure | No request; the inbound key is answered `not-acceptable`; the inbound stream closes unauthenticated |
| Outbound receiving server | Certificate refused, or none | Active failure | The attempt fails and backs off; no key is ever sent over an unverified connection |

- **When a certificate may be absent.** A receiving TLS connection lets
  the client present no certificate only where the peer may then use
  dialback: when `outbound` is on (the peer may be a Receiving Server
  asking this deployment to verify a key) or when the pair may use inbound
  dialback (the peer may be an Initiating Server for a listed domain). The
  claimed domain is known from the pre-TLS header, so this is decided
  before the handshake. Everywhere else a certificate stays mandatory, as
  before this slice. This settles the connection-manager design's
  follow-up on absent client certificates.
- An absent certificate is detected after a completed handshake in which
  the peer sent no certificate and the verifier was never asked about
  one (`PeerVerifier::completed_without_certificate`). A certificate that
  was presented always reaches the verifier, and a refusal ends the
  handshake whether or not a certificate was required, so an active
  failure can never be read as an absent certificate.
- A peer whose certificate names a different domain (a hosting provider's
  certificate, for example) is an active failure under D2 and cannot use
  dialback, although XEP-0178 would allow a receiving server to continue
  with dialback when the certificate does not match. This is the D2
  restriction; the harness slice (W10) measures what it costs with
  Prosody and ejabberd.
- Logs carry `VerifyError::code` at debug level; metrics count
  `outcome="certificate"` connections and dialback results by role.

## Authentication strength

`VerifiedPeerStream::strength()` is `dialback` for every stream whose pair
authenticated by dialback: an inbound stream authenticated as Receiving
Server, even when the peer also presented a verified certificate, and an
outbound stream on which Waddle authenticated itself as Initiating Server
after verifying the remote certificate. `pkix` is reserved for TLS
certificate verification followed by SASL EXTERNAL. An inbound dialback
stream without a certificate carries no certificate evidence:
`trust_profile()`, `matched_identifier()`, `certificate_sha256()` and
`expires_at()` are `None`, and only the maximum connection age bounds it.
The strength travels with the stream to the routing slice and the
protected-effect witness (G17); D8 forbids privileged authority and room
moderation on `dialback` streams, which those slices enforce.

## Residual risk

- **Keys are not bound to the TLS channel.** A party that obtains the key
  an Initiating Server issued for a stream ID is accepted as that domain
  for that stream. On Waddle's own inbound streams the Authoritative
  Server's certificate is verified, so a poisoned DNS answer leads to a
  failed verification rather than an impostor; the residual exposure is a
  man-in-the-middle position between the claimed domain's Initiating
  Server and Waddle against an Initiating Server that does not validate
  Waddle's certificate, which then hands its key to the attacker. In the
  other direction, a peer that accepts Waddle by dialback and does not
  validate the certificate of the server it reaches as Waddle's
  Authoritative Server can be deceived by DNS; that is the peer's
  exposure, not something Waddle can close.
- **The secret.** Anyone holding the deployment's secret can authenticate
  as every local domain to every peer that accepts dialback, until the
  secret is rotated on every replica. The secret is therefore treated like
  a private key: an absolute path, size-bounded reads, wiped buffers, and
  never logged.
- **Weaker identity.** Dialback proves control of the domain's DNS and of
  a server that holds a certificate for it, never a certificate on the
  stream itself. D8 and host policy limit what a `dialback` stream can do;
  inbound dialback is limited to operator-listed domains.
- **Oracle and amplification.** Stateless verification answers any
  well-formed request after TLS; HMAC-SHA256 makes that useless for
  forging keys, and the per-stream request cap and the unauthenticated
  socket caps bound the work. A remote party can make Waddle open one
  verification connection per unauthenticated inbound socket, only to the
  Authoritative Server of a listed domain, within the global dial rate.

These risks are accepted for the dark-by-default implementation. Enabling
inbound dialback on a deployment is a G5 decision that needs this
document's review.

## Recorded deviations and restrictions

- A dialback error closes the stream (above); XEP-0220 §2.5 keeps it open
  for other pairs, which this profile never carries.
- Dialback runs only over TLS; plaintext dialback, which XEP-0220 permits,
  is refused in every role.
- A certificate that does not match the claimed domain is an active
  failure and never leads to dialback (D2, stricter than XEP-0178).
- No sender or target multiplexing; no XEP-0288 bidirectionality.
- No periodic re-verification of a dialback stream; its maximum age ends
  it.

## Follow-ups

- W10: dialback in both directions against the pinned Prosody and
  ejabberd, including peers with mismatched certificates and peers that
  verify keys on an existing authenticated stream.
- Overlapping secrets for rotation without failed negotiations.
- Routing (W5a) and the effect boundary (G17): honour the `dialback`
  strength as D8 requires.
