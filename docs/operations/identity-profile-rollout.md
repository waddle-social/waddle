# Identity profile rollout (#2047)

This release enforces the identity profile `waddle-identity-v1`
([design](../../server/docs/planning/identity-profile-2047.md)). It changes
behaviour with server-to-server federation off: client stanzas, sign-in,
provisioning and account lookup no longer fold a spelling onto another
account, and configured domains must be canonical. It is the identity
profile's part of the rollout gate G24 of
[ADR-0020](../../server/docs/adrs/0020-independent-community-federation.md);
the federation rollout runbook collects it with the other parts. The
[authorization ID codec cutover](authz-codec-cutover.md) ships in the same
rollout and narrows the codec's user subjects to the same canonical form.

## What changes for clients and accounts

| Area | Before | After |
| --- | --- | --- |
| Stanza `to`/`from`, payload `jid` attributes, MUC invitation and decline `to`/`from`, and data-form fields typed `jid-single`/`jid-multi` | case, width and compatibility forms folded by the JID parser | refused with `<jid-malformed/>`; a trailing dot is removed and a U-label domain becomes its A-label form |
| MUC nicknames and other resourceparts in stanza addresses | Resourceprep (NFKC) folded fullwidth and compatibility characters, ligatures and code points mapped to nothing (`room@muc/ｎｉｃｋ` joined as `nick`) | the whole stanza is refused with `<jid-malformed/>`; a client must use the nickname exactly as it is to appear |
| Stanzas to numeric IPv4 domains (`2130706433`, `0x7f.1`, `127.1`) | classified as remote or unqualified domains | classified as numeric IPv4 forms, never routable; IQ requests are answered `<remote-server-not-found/>` as for IP literals and single-label names. Messages and presence to these domains are not classified: like stanzas to any other domain this server does not serve, a message with no local recipient is dropped without an error reply |
| XEP-0198 handled count | a stanza refused at parsing was not counted, so a client replayed it after resumption and later acknowledgements named the wrong stanza | every well-formed stanza refused at parsing counts as handled |
| RFC 7395 `<open from>` | folded | a non-canonical hint closes the stream with `invalid-from` |
| SCRAM username | `ALICE` signed in as `alice` | only the exact canonical name signs in |
| OAUTHBEARER | the stored localpart was re-slugged and lowercased | the stored bare JID and localpart must name the account exactly; authzid must be canonical |
| Resource binding | `ｗｅｂ` bound as `web` | `<bad-request/>` |
| New accounts (fixed accounts, OIDC; XEP-0077 registration exists only in test builds) | any valid localpart; OIDC slugs kept Unicode letters | 1–64 bytes of `a-z`, `0-9`, `.`, `_`, `-`, `+`; OIDC slugs transliterate Latin letters (`José` → `jose`, `Øyvind` → `oyvind`) and drop other characters. The name is taken from the first source whose slug keeps a character: the `preferred_username` claim, then the claim the provider's `username_claim` names (for example `login` for GitHub), then the local part of the email claim, then `ext_<provider>_<digest>` |
| Admin ad-hoc commands (`group-dm:create`, `channels:set-affiliation`, `channels:kick`, `spaces:set-role`, push operator commands) | JID fields folded by the parser | a JID that is not canonical is refused: `<jid-malformed/>` for a field submitted with type `jid-single` or `jid-multi`, `<bad-request/>` for one submitted without a type |
| Account lookups (roster, presence, group DMs, XEP-0055) | the input was folded before the key lookup | the input is taken exactly; a non-canonical spelling names no account |
| XEP-0055 results | stored names folded into addresses | rows whose stored name is not canonical are left out; a legacy `Bob` row does not hide the canonical `bob` |
| Configured domains | any JID-valid spelling | `WADDLE_XMPP_DOMAIN`, `WADDLE_MUC_DOMAIN` and `WADDLE_SPACES_JID` must be written lowercase, without a trailing dot and in A-label form, or configuration load or startup fails |
| `WADDLE_SERVER_OWNER_LOCALPARTS` | entries lowercased; `@name` and `name@domain` accepted | each entry must be the exact canonical localpart (no `@`, no domain), or startup fails; an account is an owner only when its stored name equals an entry exactly |
| Bookmark item ids: XEP-0503 Space items, XEP-0402 PEP bookmarks (`urn:xmpp:bookmarks:1`) and Waddle DM bookmarks (`urn:waddle:dm-bookmarks:0`) | stored as written; the parser folded the id to the room or contact (`Room@muc.example.com` named `room@muc.example.com`) | publishing an id that is not canonical is refused with `<bad-request/>`, including a remote room on an internationalized domain written in U-label form (`room@bücher.example`): clients must store such a bookmark under the A-label form (`room@xn--bcher-kva.example`). A retract with a non-canonical id is accepted only when an item with exactly that id is stored, so existing items can be removed (see below) |

Nothing is written to the account stores by this release beyond what it
already wrote; there is no migration and no schema change.

## Existing accounts outside the profile

At startup each replica reads both account stores and logs, without account
identifiers, either `every local account is inside the identity profile` or
`local accounts outside the identity profile` with a count per finding:

- `non_canonical_spelling`: the stored name is one preparation would change
  (for example `Bob`, `straße`, or an OIDC account whose stored JID is not
  its name's canonical address). **OIDC accounts with this finding can no
  longer sign in over XMPP.** Native accounts with it sign in only under the
  canonical spelling their lookup key records (`bob`), and receive no
  bootstrap membership from the authorization store reseed, as before.
- `outside_account_profile`: canonical but not ASCII-profile (for example
  `josé`). These accounts keep working. No new account can take such a name,
  and each needs an operator decision before remote identities or grants are
  enrolled (S2S routing and grant authority).
- `missing_key`: rows a node predating lookup keys wrote. The periodic
  backfill keys them within five minutes; until then they are not found.
- `key_mismatch`: a recorded key that is not the stored name's key. Lookups
  resolve the row under another name; investigate before activation.
- `folded_collision`: two or more accounts that claim one address across
  the OIDC and native stores, either because their names fold to it or
  because an identity provider provisioned an OIDC account (a `users` row
  with an `auth_identities` row) under a native account's exact name. At
  most one of them holds the address. A native account and an OIDC row with
  its exact name that no identity provider provisioned (a `users` row with
  no `auth_identities` row) are one account and are not reported.
- `foreign_domain`: native rows for another domain; not accounts of this
  deployment.

For the per-account evidence behind the counts, run the read-only
[`waddle-identity-audit`](../../server/docs/operations/identity-audit.md)
against the same database and keep its report private. Its findings
correspond to these categories: `changed_by_current_parser` and
`invalid_current_parser_identity` to `non_canonical_spelling`;
`unicode_input` and `candidate_localpart` on an otherwise canonical name to
`outside_account_profile`; its collision groups to `folded_collision`;
`missing_prepared_key` to `missing_key`; and `incompatible_prepared_key` to
`key_mismatch`. Its diagnostic candidate differs from this profile in two
ways: it excludes `+`, which this profile allows, and it flags A-label
domains, which are this profile's canonical domain form.

## Stored bookmark items with non-canonical ids

Bookmark items stored before this release can have ids that are not
canonical. Each still holds what its publish gave it: a Space item gives the
channel its id folds to (`Room@muc.example.com` folds to
`room@muc.example.com`) a parent grant on that Space, so the Space's admins
administer the channel, and a PEP or DM bookmark feeds its owner's
notification setting for the folded room or contact. Publishing to that id
is now refused, so these items are not updated in place.

At startup each replica reads the PubSub store and logs, without item ids or
owners, either `every stored bookmark item id is canonical` or `stored
bookmark items with non-canonical ids` with a count per carrier: `spaces`,
`pep_bookmarks` and `dm_bookmarks`.

To clear them, retract each item by its exact stored id:

- A Space item: an owner of that Space, or an account allowed to create
  Spaces, sends a XEP-0060 retract to the Spaces service for that node and
  id. The channel's parent grant on
  that Space and its channel-space link to it are removed with the item,
  unless an item under the room's canonical id is on the same node, which
  keeps the grant. `spaces:delete` of a Space holding such an item removes
  the item, its parent grant and a link to that Space, but destroys the
  channel only when an item under the room's canonical id is on that Space
  too; a channel only a non-canonical item names survives the delete and
  can be removed with `channels:delete` if it belongs to that Space.
- A PEP or DM bookmark: its owner retracts it from `urn:xmpp:bookmarks:1` or
  `urn:waddle:dm-bookmarks:0` by its exact id. The notification setting it
  fed is cleared, or, when a bookmark under the canonical id remains,
  derived again from that bookmark. Clients that kept a non-canonical
  spelling should republish the bookmark under the canonical id.

Before enabling remote identities or grants, the `spaces` count must be zero
or each remaining Space item accepted: a remaining item keeps its parent
grant.

## Procedure

1. Before the window, confirm the configured domains and owner entries are
   canonical. A replica refuses to start otherwise, naming the variable:
   `WADDLE_MUC_DOMAIN` or `WADDLE_SPACES_JID` at configuration load,
   `WADDLE_XMPP_DOMAIN, WADDLE_MUC_DOMAIN and WADDLE_SPACES_JID must be
   canonical domains` at startup, or a `WADDLE_SERVER_OWNER_LOCALPARTS
   entry` error. Owner entries are bare localparts exactly as the accounts
   are named (`rawkode`, not `Rawkode` or `rawkode@waddle.social`).
   Provision each owner account before enabling identity provider sign-up:
   an OIDC name is transliterated into the account profile, so a sign-up
   whose name transliterates to an owner entry (`Rawködé` → `rawkode`) can
   take that name first if the owner account does not exist yet. OIDC
   provisioning checks only the OIDC store, so an owner that is a native
   account is reported as `folded_collision` if an identity provider later
   provisions its name.
2. Run `waddle-identity-audit` against the account database of the running
   release and decide, for every `non_canonical_spelling` OIDC account, how
   its owner will continue (this release does not rename accounts). Record
   the counts so the startup log can be compared.
3. Roll out with the authorization codec cutover. Every replica must run this
   release before any client relies on the refusals: a replica of the
   previous release still folds inputs, accepts `ALICE` at sign-in and
   provisions Unicode OIDC names. Accounts such a replica provisions during
   the rollout appear in the next startup report.
4. After the rollout, compare each replica's startup counts with step 2. The
   counts are the same on every replica of one database.
5. Before enabling remote identities or grants (S2S routing, grant
   authority), resolve or explicitly accept every account the report lists,
   and clear or explicitly accept every bookmark item the bookmark report
   counts.

## Rollback

The previous release reads the same stores and nothing this release writes is
incompatible with it, so a binary rollback needs no data change. Rolling back
re-enables folding at every boundary listed above; accounts provisioned by
this release remain valid under the previous one. A rollback of this release
alone is constrained by the authorization codec cutover it ships with; follow
that runbook's rollback section.

These instructions do not authorize or assert a deployment.
