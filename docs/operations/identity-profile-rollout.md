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
| Stanza `to`/`from` and payload `jid` attributes | case, width and compatibility forms folded by the JID parser | refused with `<jid-malformed/>`; a trailing dot is removed and a U-label domain becomes its A-label form |
| RFC 7395 `<open from>` | folded | a non-canonical hint closes the stream with `invalid-from` |
| SCRAM username | `ALICE` signed in as `alice` | only the exact canonical name signs in |
| OAUTHBEARER | the stored localpart was re-slugged and lowercased | the stored bare JID and localpart must name the account exactly; authzid must be canonical |
| Resource binding | `ｗｅｂ` bound as `web` | `<bad-request/>` |
| New accounts (XEP-0077, fixed accounts, OIDC) | any valid localpart; OIDC slugs kept Unicode letters | 1–64 bytes of `a-z`, `0-9`, `.`, `_`, `-`, `+`; OIDC slugs drop other characters |
| Account lookups (roster, presence, group DMs, XEP-0055) | the input was folded before the key lookup | the input is taken exactly; a non-canonical spelling names no account |
| XEP-0055 results | stored names folded into addresses | rows whose stored name is not canonical are left out |
| Configured domains | any JID-valid spelling | `WADDLE_XMPP_DOMAIN`, `WADDLE_MUC_DOMAIN` and the domain of `WADDLE_SPACES_JID` must be lowercase, without a trailing dot and in A-label form, or startup fails |

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
- `folded_collision`: two or more accounts whose names fold to one address,
  across the OIDC and native stores. At most one of them holds the address.
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

## Procedure

1. Before the window, confirm the configured domains are canonical. A
   replica refuses to start otherwise with `WADDLE_XMPP_DOMAIN,
   WADDLE_MUC_DOMAIN and WADDLE_SPACES_JID must be canonical domains`.
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
   authority), resolve or explicitly accept every account the report lists.

## Rollback

The previous release reads the same stores and nothing this release writes is
incompatible with it, so a binary rollback needs no data change. Rolling back
re-enables folding at every boundary listed above; accounts provisioned by
this release remain valid under the previous one. A rollback of this release
alone is constrained by the authorization codec cutover it ships with; follow
that runbook's rollback section.

These instructions do not authorize or assert a deployment.
