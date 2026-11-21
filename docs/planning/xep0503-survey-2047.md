# Issue 2047: who implements XEP-0503

Survey date: 2026-11-16. It repeats the survey that
[ADR-0020 D4](../../server/docs/adrs/0020-independent-community-federation.md#decision-spaces-d4)
recorded as unverified, before the acceptance criteria for Spaces
interoperability are fixed.

XEP-0503 "Server-side spaces" is pinned for this work at version 0.3.0
(xsf/xeps `4132c531487fe9450ae90b905b8df950d0d478b2`, `xeps/xep-0503.xml`).

## Method and limits

Everything here was read from public sources on the survey date; nothing was
run. Each statement carries one of three labels:

- **read**: read on the page or in the source file named;
- **snippet**: seen only in a search result;
- **unverified**: could not be reached or confirmed.

A negative ("no implementation") means the following and no more: the
project's default-branch source archive was searched, case-insensitively, for
`urn:xmpp:spaces`, `xep-0503` in its common spellings, `spaces:avatar`,
`spaces:banner` and `server-side spaces`, and its DOAP file and public XEP
list, where it has them, name no XEP-0503 entry. The 95 DOAP files that
`data/software.json` of xmpp.org registers were swept the same way (94
fetched). No global code search was available (grep.app refused the requests,
and Sourcegraph missed a known implementation), so small projects that are
not registered at xmpp.org may be missing. The feature tables are static
reads of source at the revisions named, not live `disco#info` answers.

## Result

- No XMPP server ships XEP-0503 support, and none claims it: Prosody (trunk
  and community modules), ejabberd (and ejabberd-contrib), Openfire, Tigase,
  MongooseIM and Snikket (all **read**).
- The implementations are **Movim** (client) and **Slidge** (gateway library
  and three of its gateways), as the earlier survey said, and two small
  clients it missed, **chord** and **shitcord67**. Movim and Slidge are not
  independent of the specification: its two authors are the maintainer of
  Slidge and the author of Movim (**read**, XEP header).
- No implementation found matches version 0.3.0 on both of the points it
  changed (the room `disco#info` field name and the avatar and banner item
  identifiers). Movim and Slidge both declare `partial` support of `0.2.0` in
  their DOAP files (**read**).
- Stock PubSub on Prosody and ejabberd can hold a node typed
  `urn:xmpp:spaces:0`, with the gaps listed below (**read**, source).

This confirms the basis of ADR-0020 D4: third-party Spaces acceptance rests
on plain XEP-0060 against stock Prosody and ejabberd PubSub and on the MUC
fallback, and Movim and Slidge results are recorded without blocking.

## The specification

- <https://xmpp.org/extensions/xep-0503.html>: version 0.3.0 of 2026-05-03,
  Experimental, Standards Track. The page has no implementations section
  (**read**).
- The history of `xep-0503.xml` in xsf/xeps has three commits: promotion to
  Experimental (2025-03-11), the rewrite onto PubSub (0.2.0, dated
  2025-09-13 in the XEP's revision history and committed 2025-09-16) and
  0.3.0 (authored 2026-05-03). No open pull request touches the file
  (**read**).
- Version 0.3.0 changes two things relative to 0.2.0: the room `disco#info`
  field becomes `muc#roomconfig_pubsub` (it was `muc#roominfo_pubsub`), and a
  section adds the avatar and banner items
  (`urn:xmpp:spaces:avatar:metadata:0`, `urn:xmpp:spaces:banner:metadata:0`)
  (**read**, diff of the 0.3.0 commit).

## Implementations

| Name | Kind | What it does | Version it claims | Label |
| --- | --- | --- | --- | --- |
| Movim | Client | Spaces since v0.33 (2026-04-24); latest release read v0.35.5 (2026-11-04). Creates, deletes and configures Spaces, publishes and retracts rooms, manages subscriptions and affiliations | DOAP: `partial`, `0.2.0` | read: `doap.xml`, `CHANGELOG.md`, `src/Moxl/Stanza/Space.php`, `app/Session.php` at github.com/movim/movim |
| Slidge | Gateway library | Spaces since v0.4.0 (2026-06-23), opt-in for each gateway; latest release read v0.5.4 (2026-10-30). Lists Spaces and their rooms, subscribe and unsubscribe; no creation, deletion, publishing or retraction. Avatar and banner items are served since v0.5.1 | DOAP: `partial`, `0.2.0`, "Users can list spaces and their rooms, but cannot administrate them." | read: `doap.xml`, `slidge/core/dispatcher/{disco,pubsub}.py`, `slidge/core/pubsub.py` at codeberg.org/slidge/slidge |
| slidcord (Discord), matteridge (Mattermost) | Gateways | Enable Slidge's Spaces since their v0.4.0 releases (June 2026) | None stated | read: release archives |
| matridge (Matrix) | Gateway | Enables Slidge's Spaces on its default branch only; not in v0.5.1 | None stated | read: archives |
| chord | Client | Creates, joins and leaves Spaces, handles `authorize` and `whitelist` access, publishes avatar and banner items with the 0.3.0 identifiers | None stated | read: `chord-core/src/features/spaces.rs` at github.com/abbyfluoroethane/chord |
| shitcord67 | Client | Carries a `urn:xmpp:spaces:0` module; its own documents disagree on whether it is supported; last change predates 0.3.0 | "XEP-0503 (Draft)" | read (search of the source only; behaviour not analysed) |
| Cheogram Android | Client | The XMPP Newsletter of April 2025 reports an initial implementation in 2.17.10-1, which would be the pre-PubSub 0.1.0. No trace in the source mirror at that tag or on its default branch, and no DOAP entry | — | newsletter: read; implementation: unverified |

Other Slidge gateways read (WhatsApp, Telegram, Signal, Steam, Facebook
Messenger, Skype, QQ, voip.ms) do not enable Spaces on their default
branches (**read**).

### How Movim, Slidge and chord use the protocol (**read**, source)

| | Movim | Slidge | chord |
| --- | --- | --- | --- |
| Where Spaces live | A plain XEP-0060 service that must be named `spaces.<the user's host>` | The gateway component's own JID, emulated for each user; listing needs a XEP-0462 type filter | Any PubSub service; uses XEP-0462 and XEP-0499 where advertised |
| Node type | Creates nodes with `pubsub#type` `urn:xmpp:spaces:0`, `access_model` `authorize`, `publish_model` `publishers` | Reports the type in the node's meta-data form | Creates nodes of the type with `open`, `authorize` or `whitelist` access |
| Room link field | Writes `muc#roomconfig_pubsub` into the room configuration; reads `muc#roominfo_pubsub` from room `disco#info` (the 0.2.0 name) | Emits `muc#roominfo_pubsub` (the 0.2.0 name) and the `urn:xmpp:spaces:0` feature | Writes `muc#roomconfig_pubsub` where the form offers it |
| 0.3.0 image item identifiers | Absent; a Space's avatar uses the XEP-0084 node names as item identifiers | Present since v0.5.1, served only | Both present |
| Space URI | `xmpp:<service>?;node=<node>` | `xmpp:<gateway>?node=<node>` | `xmpp:<service>?;node=<node>` |

Movim states that it implements only the `authorize` flow, that Spaces were
"only fully working and tested with the ejabberd server" (2026-03-05) and
that Prosody support was to follow (2026-04-24); its wiki documents adding a
`spaces.@HOST@` host to ejabberd's `mod_pubsub` (**read**, Movim blog and
wiki).

What this means for Waddle:

- Waddle Spaces are `open` or `whitelist` nodes; the `authorize` access
  model, which Movim creates and requires, is not implemented. A Movim
  client would not create or join Spaces on a Waddle Spaces service as it
  stands. This does not block (ADR-0020 D4).
- A Movim account looks for its Spaces service at `spaces.<its own host>`,
  so it would not discover a Space on another server's service through its
  own interface.
- Movim and Slidge read or emit the 0.2.0 room field name. Waddle emits the
  0.3.0 name, `muc#roomconfig_pubsub`, as the pinned version requires.

## Servers

No dedicated module and no XEP-0503 claim (**read** for each):

- **Prosody**: no match in trunk (revision `68c7b416d9b0`, 2026-11-09) or in
  the community modules repository (revision `a46d5a075fbe`); nothing in the
  published XEP list, the `mod_pubsub` documentation or the DOAP.
- **ejabberd**: no match in the default branch (`1e6d17de8898`) or in
  ejabberd-contrib; nothing in the DOAP, the changelog or the release notes
  from 25.04 to 26.09.
- **Openfire**: no match in the default branch. Two open, unmerged pull
  requests (#3420 and #3445) describe themselves as groundwork for an
  out-of-tree Spaces plugin; the plugin itself was not found (**unverified**).
- **Tigase**, **MongooseIM**, **Snikket**: no match, and their DOAP files
  are silent. MongooseIM's current `mod_pubsub` serves PEP only.

### Stock PubSub against the XEP's "Discovering support" list

"Advertised" means that the feature string is in the source's feature list.

| | Prosody 13.0 | Prosody trunk | ejabberd (default branch, `flat` node plugin) | Openfire (default branch) |
| --- | --- | --- | --- | --- |
| The eleven required features | Ten advertised; `manage-subscriptions` is not, though the owner's subscription requests are handled | As 13.0 | Ten advertised; `meta-data` is not, though node `disco#info` returns a meta-data form | Eleven advertised |
| `pubsub#type` can be set | Yes | Yes | Yes; exposed in `disco#info` since 23.01 | Yes |
| Access models | `open` and `whitelist` only | `open`, `whitelist` and `authorize` (unreleased) | `open`, `whitelist` and `authorize` | Not checked |
| `member` affiliation | Advertised by default | As 13.0 | Yes | No (pull request #3445 is open) |
| `retrieve-affiliations` | Not advertised | Advertised | Advertised | Advertised |
| `get-pending` | No | No | Yes | Yes |
| Room field naming a PubSub node | None | None | `muc#roomconfig_pubsub` in the room configuration; `disco#info` emits the 0.2.0 name | None |

All **read** from source at the revisions above. A third party's test notes
(chord, 2026-09-28) report the same Prosody 13.0 behaviour: an `authorize`
node is refused, a `whitelist` node typed `urn:xmpp:spaces:0` works, and the
owner's subscription requests work although the feature is not advertised.
Those notes were read, not reproduced.

Consequences for the interoperability matrix of the federation plan:

- "Waddle client uses a Space hosted on the peer's PubSub service" needs,
  on stock Prosody 13.0 and on ejabberd, an `open` or `whitelist` node typed
  `urn:xmpp:spaces:0` with XEP-0402 items. Both servers can hold one.
- "Peer account reads a Waddle Space with plain XEP-0060" needs item
  retrieval and subscription from a remote account, which is what the
  Spaces service answers to remote principals.
- A client that decides what a service is from its advertised features
  finds `manage-subscriptions` missing on Prosody and `meta-data` missing on
  ejabberd; Waddle clients must not require the full list of a remote
  service.

## Clients and libraries without an implementation

No match in the source and no DOAP entry (**read**): slixmpp (Slidge
implements the XEP itself and keeps the plugin name commented out), poezio,
Gajim and python-nbxmpp, Dino, Conversations, Monal, Libervia, xmpp.js,
xmpp-rs, Smack, Converse.js, Kaidan, profanity, Prose, monocles chat,
StanzaJS, mellium.im/xmpp, the Snikket SDK and XOWS.

Mentions that are not implementations (**read**): Gajim feature request
#12298, open since 2025-05-12; Fluux Messenger's README, which says that it
does not implement Spaces yet and is watching the specification; an open
proposal for Space invitation URIs in modernxmpp/easy-xmpp-invitation.

## Listings

- Of the 95 entries of <https://xmpp.org/software/> that carry XEP data, only
  Movim and Slidge list XEP-0503 (**read**).
- The latest compliance suite, XEP-0479 (2023), does not mention it
  (**read**).
- The XMPP Newsletters from September 2025 to October 2026 mention it only
  in the specification's own version notices and for Movim and Slidge; no
  server announcement (**read**).

## Not reached

- The canonical Cheogram repository (bot challenge), and tag pages of its
  mirror.
- grep.app (requests refused) and Sourcegraph (results unreliable).
- The Openfire Spaces plugin the two pull requests refer to.
- Nothing was run: no node was created on a Prosody or ejabberd service and
  no client was pointed at one. The federation plan's interoperability
  harness is where that happens.

## Sources

Read on 2026-11-16:

- Specification: <https://xmpp.org/extensions/xep-0503.html>, its 0.2.0 and
  0.1.0 attic copies, and the history and 0.3.0 commit of `xep-0503.xml` at
  <https://github.com/xsf/xeps>.
- Listings: <https://xmpp.org/software/>, `data/software.json` of
  <https://github.com/xsf/xmpp.org> and the DOAP files it names;
  <https://xmpp.org/extensions/xep-0479.html>; the monthly XMPP Newsletters
  at <https://xmpp.org/categories/newsletter/>.
- Prosody: <https://prosody.im/doc/xeplist>,
  <https://prosody.im/doc/modules/mod_pubsub>, <https://modules.prosody.im/>,
  and the source archives and `plugins/mod_pubsub`, `util/pubsub.lua` and
  `doc/doap.xml` at <https://hg.prosody.im/trunk> and
  <https://hg.prosody.im/prosody-modules>.
- ejabberd: <https://github.com/processone/ejabberd> (`src/mod_pubsub.erl`,
  `src/node_flat.erl`, `src/mod_muc_room.erl`, `CHANGELOG.md`,
  `ejabberd.doap`), <https://github.com/processone/xmpp> (`specs/`),
  <https://github.com/processone/ejabberd-contrib>,
  <https://docs.ejabberd.im/>.
- Other servers: <https://github.com/igniterealtime/Openfire> (source and
  pull requests #3420 and #3445), the Tigase server, PubSub and MUC
  repositories, <https://github.com/esl/MongooseIM> and
  <https://esl.github.io/MongooseDocs/latest/>, the Snikket server
  repository.
- Movim: <https://github.com/movim/movim> (default branch and tags v0.32.1,
  v0.33 and v0.35.5), its wiki and its announcements at <https://mov.im/>.
- Slidge: <https://codeberg.org/slidge/slidge> (default branch and tags
  v0.3.11 to v0.6.0alpha0), its gateways' repositories, and
  <https://slidge.im/blog/2026/06/14/slidge-v0.4.0beta-spaces/>.
- Clients and libraries: the default-branch archives of the projects named
  above, <https://github.com/abbyfluoroethane/chord>, the Fluux Messenger
  README, and the issue trackers cited.
