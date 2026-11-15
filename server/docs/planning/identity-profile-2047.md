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
   hyphen restrictions (`CheckHyphens`) and DNS length limits.
3. **The canonical representation is the lowercase A-label form**, for example
   `xn--bcher-kva.example`. A U-label spelling and its A-label spelling are
   the same canonical domain.
4. An IPv4 address keeps its dotted-quad text and an IPv6 address its
   bracketed RFC 5952 text.
5. Every canonical domain has exactly one kind (`DomainKind`, below). No name
   of kind `Dns` ends in a number, so no DNS name is another spelling of an
   IPv4 address.

### Domain classification

ADR-0020 D3 refuses, for server-to-server use, both IP-address forms of RFC
7622 §3.2, every other name a resolver or address parser reads as an IPv4
address, and unqualified single-label names. The profile performs that
classification once, as `CanonicalDomain::kind`, a function of the canonical
text alone:

| `DomainKind` | Rule, in order | Examples | Names a peer |
| --- | --- | --- | --- |
| `Ipv6` | bracketed RFC 3986 `IP-literal` | `[::1]` | no |
| `Ipv4` | dotted-quad `IPv4address` (no leading zeros) | `127.0.0.1` | no |
| `NumericIpv4Form` | the final label is a number: non-empty and all decimal digits, or `0x` followed by zero or more hexadecimal digits | `2130706433`, `0x7f.1`, `127.1`, `0x7f000001`, `0177.0.0.1`, `01.2.3.4`, `1.2.3.0x4`, `example.123` | no |
| `SingleLabel` | one label | `localhost`, `intranet` | no |
| `Dns` | anything else: two or more labels, the final one not a number | `example.org`, `0x7f.example`, `example.0xg` | yes |

The `NumericIpv4Form` rule is the WHATWG URL Standard's "ends in a number"
check. It covers every form `inet_aton` and URL parsers accept (decimal,
octal with a leading zero, hexadecimal, and the one-, two- and three-part
abbreviations) and also refuses names such as `example.123` that a URL
parser rejects outright. It is applied to the canonical text, which is
lowercase; an input written `0X7F.1` is refused like any other non-canonical
spelling, while operator configuration prepared with `CanonicalDomain::prepare`
maps it to `0x7f.1` and classifies it. A numeric form keeps its own text and
is never rewritten to the dotted quad a resolver would read.

These domainparts are syntactically valid, so they parse: a client stanza
addressed to `bob@0x7f.1` is not refused with `<jid-malformed/>`. Destination
classification (`destination.rs`) maps each refused kind to an
`InvalidDestination` and answers an IQ request (`get` or `set`) with
`<remote-server-not-found/>`, as D3 requires. Only IQs are classified there.
Messages and presence a client addresses to such a domain are answered
with `<remote-server-not-found/>` as well, before any local handler sees
them ([routing design](s2s-routing-2047.md#egress)). `DomainKind::names_federation_peer` is the predicate that
admission (W4c), discovery (W4d) and S2S routing (W5a) use; none of them
re-derive the classification from text. Configured served domains may be of
any kind (`localhost` is the default account domain).

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

Three constructors express where an input comes from:

- `CanonicalDomain::parse_exact` is for protocol input. Each label must be
  either its canonical A-label or its canonical U-label, with at most one
  trailing dot. Any other spelling (upper case, fullwidth characters,
  `。` separators, an uppercase `XN--` prefix, non-canonical IP text) is
  refused, never mapped.
- `CanonicalDomain::parse_canonical` is for text that must already be
  canonical: the deployment's own configured domains and stored identities.
- `CanonicalDomain::prepare` is for operator lists such as D1's admission
  lists. It applies the UTS #46 mappings (case, width) that RFC 7622 §3.2.2
  allows during enforcement. An entry that cannot be prepared is an error,
  never skipped (D1: "an entry that cannot be prepared fails configuration
  load").

`CanonicalDomain::is_within` compares label by label, as D1's denylist needs:
`muc.example.net` is within `example.net`, `badexample.net` is not. Only
`Dns` and `SingleLabel` names have subdomains; an IP literal or numeric form
is within only itself.

This deployment's own account domain and service domains
(`WADDLE_XMPP_DOMAIN`, `WADDLE_MUC_DOMAIN`, `WADDLE_SPACES_JID`) must already
be written canonically. The raw configured text is checked, before any
parser could fold it: `WADDLE_MUC_DOMAIN` and `WADDLE_SPACES_JID` when the
configuration is loaded, and `WADDLE_XMPP_DOMAIN` with the derived service
domains at startup. Every bound account JID and served address is built from
that text.

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
occupants, never accounts or grant subjects. Surrounding XML whitespace in a
bind request's `<resource/>` is trimmed before the check, and a
server-generated resource (`ws-<uuid>`) is always canonical.

The same rule applies to the resourcepart of every stanza address, which
includes MUC occupant nicknames (`room@service/nick`): a nickname or resource
that Resourceprep would change (fullwidth or compatibility characters,
ligatures, code points mapped to nothing) refuses the whole stanza with
`<jid-malformed/>`.

## Decision 4: enforcement points

Inputs are checked as XML-decoded text, before typed parsing, so a numeric
character reference (`stra&#xDF;e`) cannot hide a spelling the parser would
fold.

| Boundary | Input | Rule | Refusal |
| --- | --- | --- | --- |
| C2S frame parser (`waddle-xmpp` `protocol/frame.rs`, `identity/stanza.rs`) | stanza `to` and `from`; every unprefixed `jid` attribute in the payload; `to` and `from` of `muc#user` `<invite/>` and `<decline/>`; each `<value/>` of a data-form field typed `jid-single` or `jid-multi` | `parse_jid`; rewritten in canonical form (trailing dot removed, A-label domain) before `xmpp_parsers` sees them | `ParseError::IdentityProfile`; the server answers `<jid-malformed/>` (modify) to an IQ get/set, a message or a presence, with the stanza `id` and no addresses echoed. The refused stanza counts toward the XEP-0198 handled count |
| RFC 7395 `<open/>` | `from` | canonical bare account JID | `invalid-from` stream error |
| SASL OAUTHBEARER (`waddle-xmpp` `auth/mod.rs`) | authzid | canonical bare account JID | `invalid-authzid` |
| SASL SCRAM | username | the exact account name; credentials are looked up under exactly that name | `not-authorized`, as for an unknown user |
| Resource binding | requested resourcepart | `parse_resourcepart` | `<bad-request/>` (RFC 6120 §7.7.2.1) |
| Native provisioning (`NativeUserStore::register` and `validate_new_account`: configured fixed accounts, validated before the reset that precedes seeding; the XEP-0077 registration hook is compiled only into test builds) | username, domain | account profile; canonical domain | registration error (`not-acceptable` over XEP-0077 in test builds); a fixed account outside the profile fails startup |
| OIDC provisioning (`auth/identity.rs`) | derived name | the slug generator transliterates Latin letters and only emits account-profile names; a source with nothing usable falls through to the next (custom claim, email local part, subject digest); each candidate is checked | provisioning error |
| Directory lookup (`auth/directory.rs`) | localpart | `exact_account_jid`; a non-canonical spelling names no account | not found |
| XEP-0055 search | query | a canonical account address matches by localpart; anything else stays free text and is never parsed into an address. Rows are merged only when their stored names are identical, so a legacy `Bob` row never hides the canonical `bob` | — |
| Mediated invitations and declines (`handlers/message/muc_invite.rs`, `muc_direct.rs`) | invitee `to`, declined inviter `to` | `parse_bare_jid` / `parse_jid` exactly; a non-canonical value names no invitee or inviter | the frame parser has already answered `<jid-malformed/>` |
| Ad-hoc admin commands (`admin/channels.rs`, `admin/spaces.rs`, `admin/push.rs`) | `member_jids`, `member_jid`, `occupant_jid`, `channel_jid`, `room_jid`, `space_jid`, `owner`, including fields submitted without a `type` | `DataForm::bare_jid` / `bare_jids` (`parse_bare_jid` after trimming whitespace) or `parse_bare_jid` | `<bad-request/>`, as for any invalid field value |
| XEP-0402 bookmark item ids (`xep::xep0402::parse_bookmark`; PEP bookmark publish and retract in `handlers/iq/pubsub_dispatch.rs`; XEP-0503 Spaces publish and retract in `handlers/iq/spaces_publish.rs`, `spaces_retract.rs`; the notification-settings projection) and Waddle DM-bookmark item ids (`xep::xep_waddle_dm_bookmarks::parse_dm_bookmark`) | `<item id>`, a room or contact bare JID with a localpart | `CanonicalBareJid::parse_exact`: the id must already be the canonical text, so the stored item id, the channel-space link and the channel's parent grant all name one room | `<bad-request/>` on publish, and on retract unless an item with exactly that id is stored. A stored item whose id is not canonical is not read as a bookmark; retracting it removes the parent grant or notification setting it fed unless an item under the canonical id still holds it, and `spaces:delete` removes it without destroying the room it folds to (Decision 6) |
| Extension PubSub nodes with a `{room}` segment (`handlers/iq/extension_route_items.rs`, `server/extension_commands/pubsub.rs`) | the room segment of the node name | `CanonicalBareJid::parse_exact`, so the channel whose permissions are checked is the room the node names before a requester is made a member of that node or an extension writes to it | `<bad-request/>` on items retrieval; the extension publish is denied |
| `WADDLE_SERVER_OWNER_LOCALPARTS` (`server/bootstrap_membership.rs`) | entries | each a canonical localpart exactly as written; owner checks compare the stored localpart exactly | startup fails |
| Authorization codec (`permissions/authz_id.rs`) | user subjects | `AuthzUserJid` additionally requires `CanonicalBareJid` | `UserJidNotCanonical` |
| Destination classifier (`destination.rs`) | parsed `to` | `canonical_address` = `canonical_jid`; served domains are canonical | — |
| Configuration and startup (`server/config.rs`, `server/http.rs`) | `WADDLE_MUC_DOMAIN`, `WADDLE_SPACES_JID` as configured; `WADDLE_XMPP_DOMAIN` and derived service domains | written canonically (`parse_canonical`, `CanonicalBareJid::parse_exact`) | configuration load or startup fails |

Payload `jid` attributes cover roster, MUC admin, PubSub affiliation and
subscription items, bookmarks, blocking lists, disco items and XEP-0033
`<address jid/>` entries, which carry the identities that grants and contact
lists are keyed by. The nested identities the frame parser does not check
are listed under open items; each is either never used as an identity by a
handler or, for data forms, held to the profile by the handler that reads
it.

## Decision 5: canonical bare-JID account binding

An authenticated connection is bound to the account's canonical stored
identity, never to a folded spelling of it:

- **OIDC.** The stored `users.jid` must already be a canonical bare JID, and
  the stored `xmpp_localpart` at the configured domain must name exactly that
  JID (`localpart_to_jid` takes a stored name exactly; it neither lowercases
  nor slugs it). The authzid and stream `from`, both canonical by the rules
  above, must equal it.
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
| `non_canonical_spelling` | the stored name (or an OIDC account's stored JID) is not the canonical address of the stored name | an OIDC account cannot sign in over XMPP; a native account signs in only under the canonical spelling its `jid_key` records |
| `outside_account_profile` | canonical, but not in the ASCII account profile (for example `josé`) | keeps signing in; no new account can be provisioned with such a name |
| `missing_key` | no lookup key | missed by lookups and sign-in until the periodic backfill keys it |
| `key_mismatch` | the recorded key is not the stored name's key | lookups resolve the row under another name |
| `folded_collision` | two or more accounts claim one address across both stores: their stored names fold to it, or an identity provider provisioned an OIDC account (a `users` row with an `auth_identities` row) under a native account's exact name. A native account and an OIDC row with its exact name and no `auth_identities` row are one account | at most one can hold the address |
| `foreign_domain` | a native row for another domain | not an account of this deployment |

Bookmark items are keyed the same way. Space items, XEP-0402 PEP bookmarks
and Waddle DM bookmarks stored before the profile can carry an id that is
not canonical (`Room@muc.example.com`); each still holds the parent grant or
notification setting its publish gave the room or contact the `jid` parser
folds it to. `reconcile_bookmark_item_ids` counts them per carrier and
startup logs the counts without identifiers. Publishing to such an id is
refused, so the only way to change one is to retract it by its exact stored
id, which the Spaces and PEP retract handlers accept for stored items only.
A Spaces retract of such an item removes the channel's parent tuple on the
node only when no item under the room's canonical id remains there, so a
retract never removes a grant another item still names. The projection row
of a retracted PEP or DM item is derived again from the canonical item when
one remains, and deleted otherwise. `spaces:delete` removes such an item
with its parent tuple and a link to the deleted Space, and destroys the
room only when an item under its canonical id is on that Space too.

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
  entries with `is_within`. A list entry or stream domain whose
  `kind().names_federation_peer()` is false (IP literal, numeric IPv4 form,
  single label) is refused by kind.
- **W4d discovery.** A domain whose kind does not name a peer is never
  resolved or dialled; the check uses `CanonicalDomain::kind`, not the text.
- **W5a S2S routing.** Inbound stanzas go through `enforce_stanza_addresses`
  (or the same `parse_jid` rule) on the decoded element before typed parsing,
  and remote principals are `CanonicalBareJid` values. The stream's
  authenticated domain is compared with the stanza's canonical `from` domain.
  Outbound destinations are refused through the same `InvalidDestination`
  classification the C2S classifier uses.
- **W5b/W5c grant authority.** Grant subjects are `CanonicalBareJid` with a
  localpart (the codec's `AuthzUserJid` enforces this); SQL grant keys store
  the canonical text.

## Open items

- Full RFC 7622 conformance needs an agreed PRECIS implementation and a
  versioned migration of stored names; this profile neither claims nor blocks
  it. Validity disagreements with strict peers on non-ASCII names remain.
- Nested identities the C2S frame parser does not check, none of which a
  handler uses as an identity:
  - `to` and `from` of messages inside XEP-0297 `<forwarded/>` (carbons,
    MAM results and client forwards);
  - XEP-0033 `<address/>` entries that carry a `uri` instead of a `jid`;
  - XEP-0166 Jingle `initiator` and `responder` attributes;
  - data-form fields submitted without a `type` attribute, for example
    XEP-0313 `with`. Ad-hoc admin commands parse theirs exactly (see the
    table); any new handler that reads an untyped field as an address must
    use `DataForm::bare_jid` or `parse_jid`.
- Addresses carried in attributes other than `jid` or in element text, which
  handlers parse with the folding `jid` parser and use for an authorization
  check. XEP-0402 bookmark item ids and extension route node room segments
  reach a grant and are parsed exactly (see the table). The others do not
  reach a grant: each authorizes against the folded address, which the `jid`
  parser treats as the same entity, and writes no grant, affiliation or
  membership keyed by the raw text.
  - XEP-0272 Muji `<muji room>` (`xep/xep0272.rs`): selects the room whose
    membership gates a Jingle session and the SFU registry the participant
    joins; no grant.
  - The link-preview lookup `<scope>` (`handlers/iq/link_preview_lookup.rs`):
    a read authorization for the preview; no grant.
  - XEP-0372 mention reference `uri` and XEP-0513 mention `uri`: select the
    notification and room inbox audience of a message; no grant.
- UTS #46 mapping follows the Unicode version of the linked `idna` release;
  changing `idna`, `stringprep` or `jid` changes the accepted set and needs a
  profile version change.
- The offline `waddle-identity-audit` diagnostic candidate
  (`candidate-ascii-account-v1`) excludes `+`; this profile's account set
  includes it.
- The startup report reads the two stores one after the other, not in one
  snapshot, and reads every row; the offline audit is the snapshot-consistent
  evidence tool.
- Provisioning checks for an existing account per store: native
  registration consults `native_users`, OIDC provisioning `users`. A native
  name and an OIDC name that fold to one address, or are identical, can
  still both be provisioned; the startup report lists them as
  `folded_collision` when the OIDC row has an `auth_identities` row. An OIDC
  row with no `auth_identities` row is taken to mirror the native account of
  the same name. The only current production path that leaves such a row is
  OIDC provisioning that fails between its `users` and `auth_identities`
  writes, which are not one transaction; an identical-name collision with
  that row is not reported.

## Verification

Written with this slice and run in the verification phase:

- `crates/waddle-xmpp/tests/identity_profile.rs`: trailing dot and
  U-label/A-label equivalence, IDNA2008 versus Nameprep, refused domain
  spellings, IP literals, the domain classification with the ADR-0020 D3
  examples, label-wise coverage, localpart aliasing refusals (case, width,
  compatibility forms, `ß`, final sigma, ignorables), resourceparts, the
  account profile, frame-parser canonicalization and refusal including
  payload `jid` attributes, character references, mediated invitations and
  typed data-form fields, `DataForm::bare_jid`, and the OAUTHBEARER authzid.
- `crates/waddle-server/tests/identity_profile_accounts.rs`: canonical
  configured domains, account-profile provisioning, fixed-account
  validation before the reset deletes anything, OIDC name transliteration,
  admin command addresses, exact SCRAM and directory lookup against legacy
  rows, and the reconciliation reports, including an identity-provider
  account under a native account's exact name and bookmark items with
  non-canonical ids per carrier.
- `crates/waddle-server/tests/identity_profile_ws.rs`: SCRAM login with
  non-canonical usernames, bind with a non-canonical resourcepart,
  `<jid-malformed/>` replies including invitations and typed command fields,
  `<remote-server-not-found/>` for refused peer domain forms, trailing-dot
  canonicalization, `<bad-request/>` for non-canonical XEP-0402 PEP bookmark
  item ids on publish and retract, retract of a stored non-canonical
  bookmark with its notification setting cleared or derived again from the
  canonical bookmark, and directory search with a legacy twin, through the
  production WebSocket server.
- `crates/waddle-server/tests/xep0503_spaces_ws.rs`: Spaces publish into a
  second Space and retract refuse non-canonical room item ids, and the room
  keeps one parent space; a stored non-canonical item is retracted with its
  parent tuple while the canonical parent remains, keeps the tuple when the
  canonical item is on the same node, and `spaces:delete` of its Space does
  not destroy the room.
- `crates/waddle-server/tests/xep0492_dm_bookmarks_ws.rs`: retracting a
  non-canonical DM bookmark keeps the canonical item's projection row, and
  clears the row when it is the only item.
- `crates/waddle-xmpp/tests/xep0402_pep_native_bookmarks.rs`:
  `parse_bookmark` and `parse_bookmark_item_id` refuse item ids the `jid`
  parser would fold.
- `crates/waddle-server/tests/xep0198_refused_stanza_ack_ws.rs`: stanzas
  refused at parsing count toward the XEP-0198 handled count; non-stanza
  input does not.
- `crates/waddle-server/tests/destination_classifier.rs` (U-label and A-label
  classification, numeric IPv4 forms, unpreparable domains) and
  `crates/waddle-server/tests/authz_id_codec.rs` (canonical user subjects).
