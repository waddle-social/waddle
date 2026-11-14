# Identity profile `waddle-identity-v1` (#2047)

## Status

Decision record for gate G10 of
[ADR-0020](../adrs/0020-independent-community-federation.md) (host-grant B6
beyond the authorization ID codec) and for the canonical domain form that
gate G1 and decision D1 refer to. It is implemented in
`crates/waddle-xmpp/src/identity/` and enforced at the boundaries listed
below. Independent review of these decisions is required before the slice
merges; the evidence (the suites listed under Verification) runs in the
verification phase. Nothing here enables federation.

## Scope

The profile defines one canonical form for domains, addresses and account
names, and where inputs are held to it before the `jid` parser prepares them.
It covers client-to-server parsing, provisioning, authentication, directory
lookup, the authorization ID codec's user subjects and destination
classification. Server-to-server ingress applies the same API in the S2S
routing slice (W5a); the verifier (W4b) and connection manager (W4c) compare
reference identities and admission-list entries in the canonical domain form.

## The pinned preparation and where it diverges

`waddle-server` pins `jid` 0.12.2 exactly; `Cargo.lock` fixes `stringprep`
0.1.5 and `idna` 1.1.0, which `jid` uses. The parser prepares localparts with
Nodeprep and resourceparts with Resourceprep (RFC 3920 stringprep profiles,
Unicode 3.2 tables), and validates a domainpart with UTS #46 before preparing
it with Nameprep (IDNA2003). RFC 7622 instead requires the PRECIS
UsernameCaseMapped profile for localparts (RFC 7613, now RFC 8265), the
OpaqueString profile for resourceparts, and IDNA2008 for domainparts. No
PRECIS implementation is in the dependency tree.

Behaviour of the pinned parser that the profile has to account for:

| Input | Pinned parser | RFC 7622 |
| --- | --- | --- |
| `fußball@example.org` | `fussball@example.org` | `ß` is valid and distinct from `ss` |
| `a@faß.example` | `a@fass.example` | IDNA2008 keeps `faß` and `fass` apart (`xn--fa-hia`) |
| `λόγος@example.org` | final `ς` becomes `σ` | `ς` is kept |
| `ﬁx@example.org`, `Ⅷ@…`, `ℌello@…` | NFKC folds to `fix`, `viii`, `hello` | compatibility forms are disallowed |
| `ali\u{AD}ce@…`, `ali\u{200D}ce@…` | mapped to nothing: `alice` | disallowed |
| `ａｌｉｃｅ@…`, `Alice@…` | `alice` | width and case are mapped: `alice` |
| `a@example.org.` | the dot is kept in the text on the ASCII path and the part accessors are offset by it | the dot is stripped before any comparison |
| `a@bücher.example`, `a@xn--bcher-kva.example` | two different JIDs | one domain |

Every row except the last two is a case where preparation folds two spellings
onto one address. Folding is harmless only when everyone agrees the spellings
name the same entity; for `ß`, final sigma and compatibility forms the pinned
parser and RFC 7622 disagree, so two distinct identities can collapse.

## Decision 1: canonical domain form

A domain has exactly one canonical form, the `CanonicalDomain` type:

1. One trailing label separator is removed before anything else (RFC 7622
   §3.2). `example.org.` is `example.org`; `example.org..` is invalid.
2. A DNS name is processed under IDNA2008 through UTS #46 non-transitional
   processing with STD3 ASCII rules (letters, digits, hyphens), the IDNA2008
   hyphen restrictions (`CheckHyphens`) and DNS length limits. The final
   label must not be all digits, so no DNS name can be another spelling of an
   IPv4 address.
3. **The canonical representation is the lowercase A-label form**, for example
   `xn--bcher-kva.example`. A U-label spelling and its A-label spelling are
   the same canonical domain.
4. An IPv4 address keeps its dotted-quad text and an IPv6 address its
   bracketed RFC 5952 text. IP literals are valid domainparts for local
   addresses; destination classification and D3 refuse them as remote peers.

Why A-labels rather than U-labels:

- The pinned parser applies Nameprep (IDNA2003) to a U-label, which turns
  `faß.example` into `fass.example`, a different registrable domain under
  IDNA2008. An A-label is ASCII, which Nameprep only lowercases, so every
  IDNA2008 domain has a canonical spelling the parser leaves unchanged and
  that no other domain shares. A U-label form would make such domains
  unrepresentable in `jid` types or silently alias them.
- Certificate reference identities are compared in A-label form (RFC 9525
  §6.3), `rustls` server names are ASCII, and SRV lookups use A-labels, so the
  verifier, discovery and admission lists all compare the form they need
  without a further conversion.
- Stored identities and authorization IDs (the `v2_` codec encodes exact
  bytes) stay ASCII in their domain part and do not depend on a Unicode
  mapping table at read time.

RFC 7622 §3.2.1 asks an entity preparing a domainpart for a stanza to use
U-labels, and §3.2.3 makes the two forms compare equal. Waddle's canonical
addresses therefore carry A-labels; a peer that implements §3.2.3 compares
them correctly. Whether S2S output presents U-labels to peers that do not is
decided in the S2S routing design (W5a); `CanonicalDomain::to_unicode` gives
the presentation form.

Two constructors express where an input comes from:

- `CanonicalDomain::parse_exact` is for protocol input. Each label must be
  either its canonical A-label or its canonical U-label, with at most one
  trailing dot. Any other spelling (upper case, fullwidth characters,
  `。` separators, an uppercase `XN--` prefix, non-canonical IP text) is
  refused, never mapped.
- `CanonicalDomain::prepare` is for operator configuration. It applies the
  UTS #46 mappings (case, width) that RFC 7622 §3.2.2 allows during
  enforcement. An entry that cannot be prepared is an error, never skipped
  (D1: "an entry that cannot be prepared fails configuration load").

`CanonicalDomain::is_within` compares label by label, as D1's denylist needs:
`muc.example.net` is within `example.net`, `badexample.net` is not.

This deployment's own account domain and service domains
(`WADDLE_XMPP_DOMAIN`, `WADDLE_MUC_DOMAIN`, the domain of `WADDLE_SPACES_JID`)
must already be written canonically; startup refuses them otherwise, because
every bound account JID and served address is built from that text.

## Decision 2: localparts

**Never fold.** A localpart from any protocol input is accepted only when
Nodeprep leaves it byte-for-byte unchanged (`parse_localpart`). Because the
check compares against the linked parser's own output, every accepted
localpart is parser-stable, and the parser is injective on the accepted set:
two accepted spellings name the same entity exactly when they are equal. This
refuses every divergent fold in the table above (`ß`, final sigma,
compatibility forms, code points mapped to nothing, decomposed sequences) and
also the folds that RFC 7622 would accept (ASCII case, width).

Case and width are refused although both preparations agree on them, for two
reasons. Waddle's account stores compared raw usernames at registration
before lookup keys existed, so `Bob` and `bob` can be distinct stored rows;
accepting `Bob` as an input for `bob` would bind one row's credentials or
grants to the other's address. And a single rule, "the input is the
canonical spelling", is what every enforcement point and every later slice
can check without a PRECIS implementation. Clients address entities by the
addresses the server gives them, which are canonical.

Some Nodeprep fixed points are not valid PRECIS UsernameCaseMapped strings
(for example symbols, or Cherokee capital letters, which current Unicode
lowercases but Unicode 3.2 did not). That is a validity disagreement with a
strict RFC 7622 peer, not an alias: among accepted strings no two fold
together. It matters only for addresses Waddle does not provision, and is
recorded under open items.

**Account profile.** An account this deployment provisions has a localpart of
1 to 64 bytes of lowercase ASCII letters, digits, `.`, `_`, `-` and `+`
(`AccountLocalpart`). On that set Nodeprep, PRECIS UsernameCaseMapped with the
RFC 7622 §3.3.1 exclusions, and XEP-0106 unescaping are all the identity, so
the account has one spelling under every preparation a peer may apply. `+` is
included because existing native accounts use it (`alice+work`). The 64-byte
limit keeps every account subject far inside the codec's 765-byte limit.

## Decision 3: resourceparts

A resourcepart from protocol input is accepted only when Resourceprep leaves
it unchanged (`parse_resourcepart`); `ｗｅｂ` would otherwise bind as `web`.
RFC 6120 §7.7.2.1 recommends preparing a non-conforming requested resource
rather than refusing it, but Resourceprep's preparation is not RFC 7622's:
OpaqueString keeps `ｗｅｂ` and `web` apart, so binding `web` would bind a
different resource. A bind request is therefore answered `<bad-request/>`,
which that section permits. Resourceparts identify connections and
occupants, never accounts or grant subjects. A bind request's surrounding XML whitespace is trimmed as before,
and a server-generated resource (`ws-<uuid>`) is always canonical.

## Decision 4: enforcement points

Inputs are checked as XML-decoded text, before typed parsing, so a numeric
character reference (`stra&#xDF;e`) cannot hide a spelling the parser would
fold.

| Boundary | Input | Rule | Refusal |
| --- | --- | --- | --- |
| C2S frame parser (`waddle-xmpp` `protocol/frame.rs`) | stanza `to` and `from`; every unprefixed `jid` attribute in the payload | `parse_jid`; rewritten in canonical form (trailing dot removed, A-label domain) before `xmpp_parsers` sees them | `ParseError::IdentityProfile`; the server answers `<jid-malformed/>` (modify) to an IQ get/set, a message or a presence, with the stanza `id` and no addresses echoed |
| RFC 7395 `<open/>` | `from` | canonical bare account JID | `invalid-from` stream error |
| SASL OAUTHBEARER (`waddle-xmpp` `auth/mod.rs`) | authzid | canonical bare account JID | `invalid-authzid` |
| SASL SCRAM | username | the exact account name; credentials are looked up under exactly that name | `not-authorized`, as for an unknown user |
| Resource binding | requested resourcepart | `parse_resourcepart` | `<bad-request/>` (RFC 6120 §7.7.2.1) |
| Native provisioning (`NativeUserStore::register`: configured fixed accounts and the XEP-0077 registration hook) | username, domain | account profile; canonical domain | registration error (`not-acceptable` over XEP-0077); a fixed account outside the profile fails startup |
| OIDC provisioning (`auth/identity.rs`) | derived name | the slug generator only emits account-profile names; each candidate is checked | provisioning error |
| Directory lookup (`auth/directory.rs`) | localpart | `exact_account_jid`; a non-canonical spelling names no account | not found |
| XEP-0055 search | query | a canonical account address matches by localpart; anything else stays free text and is never parsed into an address | — |
| Authorization codec (`permissions/authz_id.rs`) | user subjects | `AuthzUserJid` additionally requires `CanonicalBareJid` | `UserJidNotCanonical` |
| Destination classifier (`destination.rs`) | parsed `to` | `canonical_address` = `canonical_jid`; served domains are canonical | — |
| Startup (`server/http.rs`) | configured domains | written canonically | startup fails |

Payload `jid` attributes cover roster, MUC admin, PubSub affiliation and
subscription items, bookmarks, blocking lists and disco items, which carry
the identities that grants and contact lists are keyed by. Other nested
identities (data-form `jid-single` and `jid-multi` values, addresses inside
forwarded messages, XEP-0033 address lists without a `jid` attribute) are not
yet checked; see open items.

## Decision 5: canonical bare-JID account binding

An authenticated connection is bound to the account's canonical stored
identity, never to a folded spelling of it:

- **OIDC.** The stored `users.jid` must already be a canonical bare JID, and
  the stored `xmpp_localpart` at the configured domain must name exactly that
  JID (`localpart_to_jid` no longer lowercases or slugs a stored name). The
  authzid and stream `from`, both canonical by the rules above, must equal it.
- **Native.** SCRAM credentials are found under exactly the presented
  username at the canonical domain, which equals the row's `jid_key`, and the
  bound JID is that same address.

## Decision 6: accounts written before the profile

The profile changes how inputs are matched, not which accounts exist. The
stores remain the authority; nothing is rewritten. `reconcile_identity_profile`
(`auth/identity_profile.rs`) reads both stores and classifies each account;
startup logs the counts without identifiers. The categories and their
consequences:

| Finding | Meaning | Consequence |
| --- | --- | --- |
| `non_canonical_spelling` | the stored name (or an OIDC account's stored JID) is not the canonical address of the stored name | an OIDC account can no longer sign in over XMPP; a native account signs in only under the canonical spelling its `jid_key` records |
| `outside_account_profile` | canonical, but not in the ASCII account profile (for example `josé`) | keeps signing in; no new account can be provisioned with such a name |
| `missing_key` | no lookup key | missed by lookups and sign-in until the periodic backfill keys it |
| `key_mismatch` | the recorded key is not the stored name's key | lookups resolve the row under another name |
| `folded_collision` | two or more accounts' stored names fold to one address, across both stores | at most one can hold the address |
| `foreign_domain` | a native row for another domain | not an account of this deployment |

Accounts outside the account profile are kept rather than locked out: their
names are Nodeprep fixed points, so no two of them fold together, and S2S is
off. Before remote identities or grants are enrolled (W5a, W5b) each such
account needs an operator decision; the rollout runbook records it. Renaming
or migrating accounts is not part of this profile.

## How later slices use it

- **W4b verifier.** Reference identities are `CanonicalDomain` values; DNS-ID
  and SRV-ID presented identifiers are compared after the same preparation,
  in A-label form.
- **W4c connection manager and D1/D8 lists.** Allowlist, denylist,
  dialback-list and trusted-home entries are loaded with
  `CanonicalDomain::prepare`, and a failure fails configuration load. Stream
  `from`/`to` domains are accepted with `CanonicalDomain::parse_exact`.
  Allowlist, dialback and trusted-home entries match with equality; denylist
  entries with `is_within`. IP-literal domains are refused by kind.
- **W5a S2S routing.** Inbound stanzas go through `enforce_stanza_addresses`
  (or the same `parse_jid` rule) on the decoded element before typed parsing,
  and remote principals are `CanonicalBareJid` values. The stream's
  authenticated domain is compared with the stanza's canonical `from` domain.
- **W5b/W5c grant authority.** Grant subjects are `CanonicalBareJid` with a
  localpart (the codec's `AuthzUserJid` enforces this); SQL grant keys store
  the canonical text.

## Open items

- Full RFC 7622 conformance needs an agreed PRECIS implementation and a
  versioned migration of stored names; this profile neither claims nor blocks
  it. Validity disagreements with strict peers on non-ASCII names remain.
- Nested identities other than `jid` attributes are not checked at C2S
  parsing.
- UTS #46 mapping follows the Unicode version of the linked `idna` release;
  changing `idna`, `stringprep` or `jid` changes the accepted set and needs a
  profile version change.
- The offline `waddle-identity-audit` diagnostic candidate
  (`candidate-ascii-account-v1`) excludes `+`; this profile's account set
  includes it.
- The startup report reads the two stores one after the other, not in one
  snapshot, and reads every row; the offline audit is the snapshot-consistent
  evidence tool.

## Verification

Written with this slice and run in the verification phase:

- `crates/waddle-xmpp/tests/identity_profile.rs`: trailing dot and
  U-label/A-label equivalence, IDNA2008 versus Nameprep, refused domain
  spellings, IP literals, label-wise coverage, localpart aliasing refusals
  (case, width, compatibility forms, `ß`, final sigma, ignorables),
  resourceparts, the account profile, frame-parser canonicalization and
  refusal including payload `jid` attributes and character references, and
  the OAUTHBEARER authzid.
- `crates/waddle-server/tests/identity_profile_accounts.rs`: canonical
  configured domains, account-profile provisioning, exact SCRAM and directory
  lookup against legacy rows, and the reconciliation report.
- `crates/waddle-server/tests/identity_profile_ws.rs`: SCRAM login with
  non-canonical usernames, bind with a non-canonical resourcepart,
  `<jid-malformed/>` replies, trailing-dot canonicalization and directory
  search, through the production WebSocket server.
- `crates/waddle-server/tests/destination_classifier.rs` (U-label and A-label
  classification) and `crates/waddle-server/tests/authz_id_codec.rs`
  (canonical user subjects).
