# `urn:waddle:dm-bookmarks:0` — DM Carrier for XEP-0492

## Overview

`urn:waddle:dm-bookmarks:0` is a Waddle-custom PEP carrier that hosts
[XEP-0492](https://xmpp.org/extensions/xep-0492.html) *Chat Notification
Settings* for **direct (one-to-one) chats**. It is the DM counterpart to
the MUC carrier (XEP-0402 PEP Native Bookmarks): XEP-0402 is
conference-only, and no XEP-defined "DM bookmark" exists.

The decision and rationale are recorded in
[ADR-009](../../../docs/adr/009-dm-notification-carrier.md). This document
is the normative wire reference.

XEP-0492 §2.1 requires `<notify>` to be *"a child of an element
identifying a specific chat by its JID, such as a XEP-0402
`<extensions>`."* The *"such as"* admits non-XEP-0402 carriers; this
carrier identifies the chat by the **PEP item id** (the contact's bare
JID), exactly as XEP-0402 does.

## Namespace and node

| | Value |
|---|---|
| Namespace | `urn:waddle:dm-bookmarks:0` |
| PEP node | `urn:waddle:dm-bookmarks:0` (node name == namespace, as in XEP-0402) |
| Item id | The contact's **bare JID** (`localpart@domain`) |
| Payload root | `<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>` |

The `:0` suffix marks the carrier as Waddle-experimental.

### Node configuration

Created on first publish via XEP-0060 `<publish-options>`, mirroring the
XEP-0402 bookmarks node:

```
pubsub#access_model   = whitelist   (private to the owner)
pubsub#persist_items  = true
pubsub#max_items      = max
pubsub#send_last_published_item = never
```

## Payload shape

`<dm-bookmark>` directly hosts a single official XEP-0492 `<notify>`
element. There is **no** `<extensions>` wrapper and **no** native field
(a DM has no autojoin / nick / password). The `<notify>` element, its
namespace (`urn:xmpp:notification-settings:1`), and its children are
byte-identical to official XEP-0492 — Waddle hosts it, it does not fork
it.

```xml
<iq type='set' id='dm-notify-1'>
  <pubsub xmlns='http://jabber.org/protocol/pubsub'>
    <publish node='urn:waddle:dm-bookmarks:0'>
      <item id='bob@example.com'>
        <dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>
          <notify xmlns='urn:xmpp:notification-settings:1'>
            <never/>
          </notify>
        </dm-bookmark>
      </item>
    </publish>
    <publish-options>
      <x xmlns='jabber:x:data' type='submit'>
        <field var='FORM_TYPE' type='hidden'>
          <value>http://jabber.org/protocol/pubsub#publish-options</value>
        </field>
        <field var='pubsub#access_model'><value>whitelist</value></field>
        <field var='pubsub#persist_items'><value>true</value></field>
        <field var='pubsub#max_items'><value>max</value></field>
      </x>
    </publish-options>
  </pubsub>
</iq>
```

### `<notify>` content

Per XEP-0492:

- Exactly one fallback setting child (`<always>` / `<on-mention>` /
  `<never>`, no `identity-*` attributes) is read by Waddle.
- Identity-scoped sibling settings (`identity-category` / `identity-type`)
  written by other clients are **preserved verbatim** (§3 ¶1).
- The optional `<advanced>` child carries finer-grained settings under
  custom namespaces; Waddle's rich XEP-0357 push-summary opt-in (#719)
  lives there as `<rich-payload xmlns='urn:waddle:push:rich:0'/>`. Foreign
  `<advanced>` children are never deleted or altered (§3 ¶1).

The `<notify>` build/parse logic is shared with the MUC carrier — the
same §3-conformant merge core (`build_merged_notify_element`) produces the
element for both `<conference>` and `<dm-bookmark>`.

## Item lifecycle — sparse / override-only

A DM-bookmark item exists **only** when the DM carries information beyond
the XEP-0492 §3 default. Concretely, an item is present iff **any** of:

- the fallback mode is not `always` (the §3 direct-chat default), **or**
- the #719 rich-payload opt-in is set, **or**
- a foreign `<advanced>` setting written by another client is present.

When the user returns a DM to plain-default (mode `always`, no opt-in, no
foreign `<advanced>`), the client **retracts** the item (XEP-0060
`<retract>`). Absence of an item == the §3 default. This keeps the node
sparse — "an item exists" means "this DM has an override."

## Server projection

Publishes and retractions to the DM node feed the shared
`notification_settings_projection` table (see
`waddle-server/src/notification_settings_projection.rs`):

- New source variant `NotificationSettingsSource::WaddleDmBookmarks` →
  node `urn:waddle:dm-bookmarks:0`.
- Ingestion derives the row with `ConversationKind::Direct` (every item
  in this node is a DM by construction).
- An empty/absent `<notify>` derives a `Delete`; a retract deletes the
  row in the same transaction (same pattern as the bookmarks /
  `urn:waddle:dnd:0` nodes).
- The push-evaluation read path (`effective_setting*`) is source-agnostic
  and unchanged — a DM row and a MUC row are indistinguishable to the
  reader, and DM/MUC JIDs never collide.

### Validation

`validate_dm_bookmark_publish`:

- item id MUST parse as a bare JID with a localpart;
- payload root MUST be `<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>`;
- the only permitted child is the XEP-0492 `<notify>` (at most one),
  validated by the shared `validate_notify_element`;
- unknown `<dm-bookmark>` children are rejected (tight shape, mirroring
  `validate_xep0402_conference_shape`).

## Cross-client coherence

This slice does **not** subscribe to PEP `+notify` headlines on the DM
node. The chat re-fetches DM bookmarks alongside MUC bookmarks on every
fresh session-ready; a change in another client reaches this client on
the next reconnect. Real-time `+notify` sync for both the MUC and DM
nodes is deferred to a dedicated follow-up.

## Relationship to other specs

- [XEP-0492](https://xmpp.org/extensions/xep-0492.html) — the hosted
  notification-settings element.
- [XEP-0402](https://xmpp.org/extensions/xep-0402.html) — the MUC carrier
  whose transport conventions this mirrors.
- [XEP-0163](https://xmpp.org/extensions/xep-0163.html) /
  [XEP-0060](https://xmpp.org/extensions/xep-0060.html) — PEP / PubSub
  transport.
- `urn:waddle:push:rich:0` (#719) — rich XEP-0357 push-summary opt-in
  carried in `<notify>`'s `<advanced>`.
