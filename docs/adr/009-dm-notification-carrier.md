# ADR-009: DM Carrier for XEP-0492 Per-Chat Notification Settings

## Status

Accepted

## Date

2026-05-31

## Context

Issue #720, a follow-up to #506 / #532. Waddle ships per-chat
notification settings (XEP-0492 *Chat Notification Settings*): a user can
set a conversation to `always`, `on-mention`, or `never`, and the setting
syncs across their clients.

XEP-0492 v0.2.0 §2.1 requires the `<notify>` element to be *"a child of
an element identifying a specific chat by its JID, such as a XEP-0402
`<extensions>`."* The shipped implementation (#532 / #534) hosts
`<notify>` inside the XEP-0402 PEP Native Bookmarks `<conference>` /
`<extensions>` item, stored in PEP node `urn:xmpp:bookmarks:1`.

XEP-0402 is **conference-only** — it covers MUC bookmarks. There is no
XEP-defined "DM bookmark" carrier. As a result, the first slice covered
MUC channels and private groups only; direct messages fell back to the
XEP-0492 §3 default (`always`) and had **no per-DM toggle**. The server
already models `ConversationKind::Direct` and resolves it at
push-evaluation time, but no carrier ever writes a projection row for a
DM, so the per-DM setting was unreachable.

This ADR records the carrier decision so per-DM mute / mentions-only /
always can ship.

### Options considered

- **(A) Wait for a future XEP carrier.** Track standards@ for a
  DM-bookmark or roster-extension proposal. Rejected: open-ended
  timeline; per-DM settings remain unavailable indefinitely.
- **(B) Define `urn:waddle:dm-bookmarks:0` as a Waddle-custom carrier.**
  A PEP node mirroring XEP-0402's *transport conventions* (PEP, item-id =
  bare JID, whitelist access) with a minimal `<dm-bookmark>` payload that
  directly hosts the official XEP-0492 `<notify>`. **Chosen.**
- **(C) Roster / private-storage extension.** Annotate the RFC 6121
  roster item or use XEP-0145 / private storage. Rejected: roster items
  have a fixed schema (servers may strip unknown children); private
  storage loses PEP sync + access-model semantics; needs a wholly
  separate code path.

No existing XEP provides a JID-keyed, structured, PEP-synced carrier for
DM-level metadata:

| Candidate | Why it cannot carry a DM `<notify>` |
|---|---|
| RFC 6121 roster (`jabber:iq:roster`) | Fixed `<item>` schema; servers may reject/strip unknown children. |
| XEP-0145 roster annotations | Free-text `<note jid=…>` in private storage — wrong shape. |
| XEP-0402 / 0048 bookmarks | Conference-only; a non-`<conference>` item in `urn:xmpp:bookmarks:1` breaks other XEP-0402 readers. |
| A future "DM bookmark" XEP | Does not exist (that is option A). |

## Decision

Adopt **(B)**: a Waddle-custom PEP carrier `urn:waddle:dm-bookmarks:0`.

XEP-0492 §2.1's *"such as"* explicitly anticipates non-XEP-0402 carriers,
and CLAUDE.md permits `urn:waddle:*` namespaces precisely when no suitable
XEP-defined shape exists. The wire spec is in
`server/docs/specs/urn-waddle-dm-bookmarks.md`.

### Compliance invariants

What makes the carrier XMPP-native is fixed by these invariants, not by
internal code organization:

1. **PEP transport** (XEP-0163), as XEP-0402 uses — not a private side
   channel.
2. **`pubsub#access_model=whitelist`** + `persist_items` + `max_items=max`
   — identical to XEP-0402's recommended node config.
3. **Item-id = the contact's bare JID** — this is the JID-identification
   mechanism XEP-0492 §2.1 requires.
4. **`<notify>` byte-identical to official XEP-0492** — same
   `urn:xmpp:notification-settings:1` namespace, same
   `<always>/<on-mention>/<never>`, same `<advanced>`. Waddle hosts it;
   it does not fork it.
5. **The wrapper element is `urn:waddle:dm-bookmarks:0`, never an official
   namespace** — Waddle-specific semantics never squat an official XEP
   namespace.

### Sub-decisions

- **Payload shape — minimal.** `<dm-bookmark>` directly hosts `<notify>`;
  no `<extensions>` wrapper. A DM bookmark has no native fields
  (autojoin / nick / password) that would justify the
  `<conference>` / `<extensions>` split, and `<dm-bookmark>` is the direct
  analogue of XEP-0402's `<conference>` payload root. Both the
  direct-child shape and an `<extensions>`-wrapper shape are equally
  XEP-0492-compliant (JID-identification comes from the item id); the
  minimal shape keeps the custom surface as small as the namespace policy
  wants.
- **Projection — shared table.** Reuse the `notification_settings_projection`
  table keyed by `(owner_bare_jid, conversation_jid)`. Add one
  `NotificationSettingsSource::WaddleDmBookmarks` variant → the DM node.
  Ingestion hardcodes `ConversationKind::Direct` (every item in this node
  is a DM by construction, symmetric with the MUC path's hardcoded
  `PrivateGroup`). The push-evaluation read path is source-agnostic and
  unchanged.
- **Item lifecycle — sparse / override-only.** An item exists **iff** the
  DM carries information beyond the §3 default — a non-`always` mode, a
  #719 rich-payload opt-in, or foreign `<advanced>` settings written by
  another client. Returning a DM to plain-default **retracts** the item
  (absence == `always`, the §3 direct-chat default). This reuses the
  existing retract → delete-projection pattern (`pubsub/item.rs`, already
  used for the bookmarks and `urn:waddle:dnd:0` nodes).
- **Rich-payload opt-in (#719).** Supported for DMs for free — it rides
  inside `<notify>`'s `<advanced>`, which the carrier and projection
  already read. The chat UI exposes the same toggle for DMs as for
  channels.
- **Cross-client coherence — match #532.** No `+notify` headline
  subscription in this slice; the chat re-fetches DM bookmarks alongside
  MUC bookmarks on every fresh session-ready into a unified JID-keyed
  cache. Real-time `+notify` sync for both nodes is deferred to a
  dedicated follow-up (the same deferral #532 made for MUC bookmarks).

## Consequences

- Per-DM XEP-0492 settings (mute / mentions-only / always) become
  reachable; #532 v2 (DM surface) is enabled.
- The custom namespace must carry a dedicated Rust test suite (CLAUDE.md
  XEP custom test-suite hard rule), since it hosts an official XEP over a
  Waddle carrier.
- If the XSF later standardizes a DM-bookmark carrier, migration is a
  bounded re-key of the projection source + a client publish-shape change;
  the `:0` version suffix signals the carrier is Waddle-experimental.

### Out of scope

- `+notify` real-time headline sync (deferred for **both** the MUC and DM
  nodes to a later slice).
- The pre-existing MUC ingestion hardcode of `ConversationKind::PrivateGroup`
  (public/private MUC conflation) is **not** addressed here.
