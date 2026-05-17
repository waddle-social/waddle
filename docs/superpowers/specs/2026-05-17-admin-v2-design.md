# Admin V2 — Spaces + Channels management, mobile-first

Status: design approved, pre-implementation
Date: 2026-05-17
Author: David Flanagan (with Claude)
Tracking PR: TBD (follows #670 admin V1)

## Problem

Admin V1 (#670) shipped the role-gated /admin route + a read-only Users panel + sidebar stubs for Spaces / Audit / Push health / Settings. The user can't actually *do* admin work yet, and there's no surface for the day-to-day operations community admins need: managing spaces, managing channels, managing membership.

V2 lights up two of those stubs (Spaces, Channels) with full read+write, and makes the whole admin surface mobile-friendly using the chat's existing mobile-drawer pattern.

## Goals

1. A community owner can manage spaces end-to-end from /admin/spaces: list, create, edit (name/description/icon), delete; view members and change their roles.
2. A community owner can manage channels end-to-end from /admin/channels: list (with space filter), create, edit (name/topic/visibility), delete; view occupants; manage affiliations (owner/admin/member/outcast); kick.
3. The entire admin surface is usable on mobile via the chat's existing `ChatMobileDrawers` pattern — sidebar becomes a slide-from-left drawer, detail views slide from right.
4. New channels default to **public** visibility. (User confirmed.)
5. All admin operations flow over XMPP via a single Waddle-namespaced ad-hoc command family (`urn:waddle:admin:*`) — no out-of-band REST, no direct DB calls from the client.

## Non-goals (V3)

- Audit-log panel (the third sidebar stub).
- Push-health panel (fourth stub).
- Admin settings panel (fifth stub).
- Bulk operations (multi-select kick, batch affiliation change).
- Moving channels between spaces (rename / re-parent).
- Banner/announcement broadcast.
- Search across users/channels at scale (lists are paginated; basic prefix search inherited from V1 is fine).
- Audit log surface for actions taken in this view (separate work).

## Decisions

| Question | Decision | Source |
|---|---|---|
| V2 scope | Read+write — full CRUD on spaces and channels; affiliations; kick/ban | User pick |
| Mobile pattern | Match chat — reuse ChatMobileDrawers + Tailwind responsive grid | User pick |
| New channel visibility default | Public | User: "Everything is public by default" |
| Member-role vocabulary (spaces) | owner / admin / member | Inferred — matches XEP-0317 hat tiers Waddle already uses |
| Channel affiliation vocabulary | owner / admin / member / outcast | XEP-0045 §5.2 |
| Kick semantic | Role-change to "none" (occupant leaves, can rejoin) | XEP-0045 §9.1 |
| Ban semantic | Affiliation-change to "outcast" via `set-affiliation` (can't rejoin) | XEP-0045 §10.2 |
| Single admin namespace | All commands under `urn:waddle:admin:*` | V1 precedent + simpler client |

## Wire protocol — fourteen new ad-hoc commands

All under `urn:waddle:admin:*`. Each is an XEP-0050 ad-hoc command with a typed `<x type='submit'>` data form for args and a typed `<x type='result'>` data form for the response.

### Spaces (6 commands)

```
urn:waddle:admin:spaces:list:0
  args:  prefix?: string, page_size?: u32 (≤200, default 50), after_cursor?: string
  result: entries: [{ space_jid, name, description?, icon_url?, channel_count, member_count }],
          next_cursor?: string

urn:waddle:admin:spaces:create:0
  args:  name: string (1–80), description?: string, icon_url?: string
  result: space: { space_jid, name, description?, icon_url? }

urn:waddle:admin:spaces:update:0
  args:  space_jid: bareJID, name?: string, description?: string, icon_url?: string
  result: space: { space_jid, name, description?, icon_url? }

urn:waddle:admin:spaces:delete:0
  args:  space_jid: bareJID, confirm: "yes"
  result: empty
  notes: deletes the space AND every channel that lived under it
         (the server cascades to MUC room destroy per XEP-0045 §10.9)

urn:waddle:admin:spaces:members:0
  args:  space_jid: bareJID, page_size?: u32, after_cursor?: string
  result: entries: [{ jid, display_name?, role: "owner"|"admin"|"member" }],
          next_cursor?: string

urn:waddle:admin:spaces:set-role:0
  args:  space_jid: bareJID, member_jid: bareJID, role: "owner"|"admin"|"member"|"none"
  result: { member_jid, role }
  notes: role="none" removes the member entirely
```

### Channels (8 commands)

```
urn:waddle:admin:channels:list:0
  args:  space_jid?: bareJID  // omit = list across all spaces
         prefix?: string
         page_size?: u32, after_cursor?: string
  result: entries: [{
            channel_jid, space_jid?, name, topic?,
            is_public: bool,                  // false = members-only
            members_only: bool,               // XEP-0045 muc_membersonly
            occupant_count: u32,
            affiliation_count: { owner: u32, admin: u32, member: u32, outcast: u32 }
          }],
          next_cursor?: string

urn:waddle:admin:channels:create:0
  args:  space_jid?: bareJID  // omit = no space association
         name: string (1–80)
         topic?: string
         is_public?: bool      // default TRUE per user decision
  result: channel: { channel_jid, space_jid?, name, topic?, is_public }

urn:waddle:admin:channels:update:0
  args:  channel_jid: bareJID, name?: string, topic?: string, is_public?: bool
  result: channel: { channel_jid, name, topic?, is_public }

urn:waddle:admin:channels:delete:0
  args:  channel_jid: bareJID, confirm: "yes"
  result: empty
  notes: destroys the MUC room per XEP-0045 §10.9

urn:waddle:admin:channels:occupants:0
  args:  channel_jid: bareJID, page_size?: u32, after_cursor?: string
  result: entries: [{
            occupant_jid,           // full JID
            real_jid?,              // XEP-0421 occupant ID surfaced for moderators
            nick,
            role: "moderator"|"participant"|"visitor",
            affiliation: "owner"|"admin"|"member"|"none"|"outcast",
            joined_at: rfc3339
          }],
          next_cursor?: string

urn:waddle:admin:channels:affiliations:0
  args:  channel_jid: bareJID,
         filter?: "owner"|"admin"|"member"|"outcast"  // omit = all four
         page_size?: u32, after_cursor?: string
  result: entries: [{ jid, affiliation, granted_at?: rfc3339, reason?: string }],
          next_cursor?: string

urn:waddle:admin:channels:set-affiliation:0
  args:  channel_jid: bareJID, member_jid: bareJID,
         affiliation: "owner"|"admin"|"member"|"none"|"outcast",
         reason?: string
  result: { member_jid, affiliation }
  notes: affiliation="outcast" = ban; ="none" = no special standing

urn:waddle:admin:channels:kick:0
  args:  channel_jid: bareJID, occupant_jid: bareJID, reason?: string
  result: { occupant_jid }
  notes: XEP-0045 §9.1 — role-change to "none"; occupant leaves but can rejoin
```

### Disco

Server's user-server disco#info advertises the 14 namespaces above. Each command also surfaces via XEP-0050 disco#items on the commands node, so a UI that wants to enumerate available admin commands can.

## ACL

Every command checks `is_community_owner(state, requesting_jid)` (from V1) and rejects with `<forbidden/>` for non-owners. No finer-grained admin tier in V2 — community-owner is the only role authorized to invoke admin commands.

## Server architecture

```
server/crates/waddle-server/src/admin/
├── mod.rs                # facade + V1 is_community_owner
├── users_list.rs         # V1
├── spaces.rs             # NEW — 6 commands, ~600 lines + tests
└── channels.rs           # NEW — 8 commands, ~800 lines + tests
```

Each command handler:

1. Parses the typed args data form into a Rust struct.
2. Re-checks ACL (defense in depth).
3. Delegates to existing internal APIs:
   - Spaces ops → existing `spaces_pubsub_seed.rs` / pubsub admin storage.
   - Channel CRUD → existing MUC actor (create/destroy is `crate::muc::*`).
   - Channel config → MUC owner protocol internal call.
   - Affiliations → MUC admin protocol internal call (server-bypass — admin doesn't need to be joined).
   - Kick → MUC actor's role-change.
4. Returns a typed response data form. No `String`-blob payloads.

The handlers register with the same XEP-0050 command-registry the V1 users-list command uses.

## Chat client architecture

### Mobile pattern reuse

Existing chat already does:
- `ChatMobileDrawers.vue` for the channel/DM nav drawer
- `UserProfileDrawer.vue` for slide-from-right details
- Tailwind `lg:` breakpoints

Admin V2 mirrors:
- AdminLayout's left sidebar becomes a slide-from-left drawer on `<lg` (hamburger toggle in the admin header).
- Each panel's detail view (space detail, channel detail) is a slide-from-right drawer that anchors to a right-pane on `lg+`.

### New components

```
chat/src/components/admin/
├── AdminLayout.vue          # V1, extend with mobile drawer state
├── AdminUsersPanel.vue      # V1, light mobile touch-up
├── SpacesPanel.vue          # NEW
├── SpaceDetailDrawer.vue    # NEW — name/description editor + members + delete
├── SpaceCreateDialog.vue    # NEW — name + description
├── ChannelsPanel.vue        # NEW
├── ChannelDetailDrawer.vue  # NEW — config editor + affiliations + occupants + delete
├── ChannelCreateDialog.vue  # NEW — name + topic + space + is_public (default true)
├── AffiliationRow.vue       # NEW — used by both SpaceDetail and ChannelDetail
└── OccupantRow.vue          # NEW — used by ChannelDetail
```

Routes:
- `/admin/spaces` → SpacesPanel
- `/admin/channels` → ChannelsPanel
- (V1) `/admin/users` → AdminUsersPanel

`navigation.ts` AdminPanel union extends to include `"spaces"` and `"channels"` (it already lists them as future stubs).

### Wasm bindings

Mirror the V1 `admin_users_list` pattern. Each command gets:
- Typed Rust args struct in `waddle-xmpp-client-wasm/src/client_admin.rs` (or split per resource as the file grows).
- `#[wasm_bindgen]` method on `WaddleXmppClient` that builds the IQ, sends it, parses the response, returns a `Promise<TypedResponse>`.
- `BrowserXmppClient` wrapper in `chat/src/lib/xmpp/client.ts` mirroring the `adminUsersList` pattern.

### Tests

- **Server:** `tests/admin_spaces_ws.rs` + `tests/admin_channels_ws.rs`. Each command × {happy, forbidden non-owner, validation rejection, edge case}. ~42 cases total.
- **Chat:** Vitest for each new panel + drawer. Cover: list renders, click row opens detail, save submits correct args, delete dialog requires confirm, mobile breakpoint shows drawer instead of pane.

## Risks and open questions

- **Cascading deletes:** spaces:delete destroys every channel under the space. Spec says yes; UI MUST require an explicit confirmation that lists what will be destroyed.
- **Admin acting on rooms they're not joined to:** kick/affiliation-change need to bypass the "must be joined and have role" check that applies to regular MUC owner IQs. The Waddle admin commands hit the MUC actor directly without spoofing membership.
- **XEP-0421 occupant-id exposure:** the `real_jid` field on channel occupants surfaces the underlying JID. This is appropriate for admin view but MUST NOT leak into other panels.
- **Concurrency on space-delete:** if a member is in a channel being cascaded, the kick is best-effort; clients reconnect.
- **Rate-limiting:** out of scope for V2; admin commands trust the owner.

## Implementation order (PR commits)

1. `feat(server): admin spaces — list + create + update + delete`
2. `feat(server): admin spaces — members + set-role`
3. `test(server): admin_spaces_ws integration matrix`
4. `feat(server): admin channels — list + create + update + delete`
5. `feat(server): admin channels — occupants + affiliations + set-affiliation + kick`
6. `test(server): admin_channels_ws integration matrix`
7. `feat(server): wasm bindings for all admin V2 commands`
8. `feat(chat): SpacesPanel + SpaceDetailDrawer + SpaceCreateDialog`
9. `feat(chat): ChannelsPanel + ChannelDetailDrawer + ChannelCreateDialog`
10. `feat(chat): AdminLayout mobile drawer + responsive grid`
11. `test(chat): spaces + channels panels`
12. `docs(server): conformance audit — admin V2 namespaces`
