# ADR-010: Presence Statuses — Availability States, Auto-Away, and the In-Call Overlay

## Status

Proposed

## Date

2026-06-24

## Context

Presence in Waddle is *experienced* as online/offline only, but that is a
reachability gap, not a data-model gap:

- The server already models the full RFC 6121 Show set —
  `Available, Away, Xa, Dnd, Chat` (`waddle-xmpp-core/src/presence/mod.rs`)
  — stores show/status/priority per resource, persists it across
  XEP-0198 resume, and relays it to roster subscribers and MUC occupants.
- The client already *renders* away/xa/dnd (green/yellow/red/grey dots in
  the DM panel, avatar, chat header, profile drawer);
  `PresenceUpdateEvent.show` is `available | away | xa | dnd | offline`.
- XEP-0319 idle (`urn:xmpp:idle:1`) is implemented server-side.

What is missing is the ability to *reach* those states:

1. **No outbound presence-publish path.** The client only *receives*
   presence; self-presence is reactive. Nobody can ever *become* away or
   dnd, which is why presence feels binary.
2. **No auto-away.** Idle/visibility is tracked (`shell/window-visibility.ts`)
   but wired only to notifications, never to presence.
3. **No manual status picker.**
4. **"In a call" is not a presence concept.** It exists only as the
   in-room `urn:waddle:in-call:0` substate (hand-raised/muted) and Muji
   (XEP-0272) participant tracking. A roster/DM contact outside the call
   room has no signal that a user is in a call.

Goal: Slack/Teams-parity presence — Away (manual + automatic) and an
"in a call" indicator — without violating the CLAUDE.md XEP-conformance
hard rule (prefer conformant XEP shapes; custom `urn:waddle:*` only where
no XEP shape fits; advertised official namespaces must conform exactly).

Note: a `urn:waddle:dnd:0` node already exists. It is a **push-notification
suppression schedule** (snooze + weekly quiet-hours), *not* the presence
`<show>dnd</show>`. The two are disambiguated in `CONTEXT.md`. This ADR is
about the presence Show; it does not touch the DND schedule.

### Options considered

The design was resolved as a sequence of forks. For each, the chosen
branch and the rejected alternatives:

- **In-call model.** *Overlay* (orthogonal to the Show, **chosen**) vs.
  *replacement* Show (Teams-style "In a call" that overrides
  Available/Away) vs. *auto-DND* (reuse `<show>dnd</show>`). Rejected
  replacement and auto-DND: both destroy the user's chosen Show and cannot
  distinguish "I'm on a call" from "I set DND" / "I'm heads-down." Overlay
  matches the existing Waddle instinct that call state sits *beside*
  presence, never inside it.
- **In-call carrier.** *Layered* (**chosen**): XEP-0108 User Activity
  for the roster-facing overlay **and** `urn:waddle:in-call:0` for the
  in-room substate. vs. XEP-0108 alone vs. extending
  `urn:waddle:in-call:0` to roster presence. The hard rule forbids a
  custom namespace where a XEP shape fits, and XEP-0108
  `<talking><on_the_phone/>` *is* "in a call" — so the roster overlay must
  be XEP-0108. The custom namespace is justified only for the substate
  (hand-raised/muted), which no XEP models.
- **Auto-away policy.** *Input-timer* (**chosen**) vs. *conservative*
  (visible+focused is always present) vs. *visibility-only*. Input-timer
  (Available → Away after ~10 min without interaction or tab hidden →
  Extended Away after ~30 min) tracks "are they interacting" most
  closely, accepting that reading-without-moving on the Waddle tab can
  read as Away. A browser cannot see OS-level idle, so all three are
  proxies; this one biases toward catching at-desk-but-gone.
- **Manual-vs-auto precedence.** *Automatic default + sticky manual +
  Reset* (**chosen**) vs. *sticky Away/DND only, no pin* vs. *manual
  expires (Teams duration)*. Automatic is the default mode (idle timer
  governs); any manual pick (Available/Away/DND) is sticky and suspends
  auto-away until "Reset to automatic." This is the only option that lets
  a user pin themselves Available against the idle timer.
- **Cross-device sync.** *Hybrid* (**chosen**): manual status synced
  account-wide and persisted; auto-away per-device. vs. *per-device
  ephemeral* vs. *fully account-global*. Per-device manual status is
  self-defeating under most-available-wins (your own active phone masks
  the Away you set on your laptop — Away loses to Available; only a
  deliberate DND is exempt, since it outranks every online state).
  Fully-global is wrong for idle (one
  idle device should not mark the whole account away). Hybrid is the only
  coherent split: idle is inherently per-device; a deliberate choice is a
  human-level intent that should follow the user.

## Decision

Adopt the overlay-based, XEP-conformant presence model below.

1. **Effective Show precedence (per device):** Manual status (if set)
   over Auto-away computation. The In-call overlay never changes the
   Effective Show.
2. **In-call overlay** is automatic (derived from joining a 1:1 or MUC
   call, retracted on leave), orthogonal to the Show, and **pauses
   auto-away** while active.
3. **Roster-facing in-call** rides **XEP-0108 User Activity**
   (`on_the_phone` / `on_video_phone`) via PEP; **in-room in-call**
   stays on `urn:waddle:in-call:0`.
4. **Auto-away** is the per-device input-timer (Available → Away → Extended
   Away), stamped with **XEP-0319** `<idle since=…/>`.
5. **Manual status** is synced account-wide and persisted via a new
   `urn:waddle:status-preference:0` PEP node (XEP-0223 persistent private
   storage); every resource adopts it; it survives reconnect. Auto-away is
   never synced.
6. **Incoming render** collapses a contact's resources with
   **most-available-wins**.

### Compliance invariants

What keeps this XMPP-native is fixed by these invariants:

1. **RFC 6121 Show values are used verbatim** — `away`, `xa`, `dnd`,
   `chat`, plain Available, `unavailable`. No new Show values are minted.
2. **XEP-0319 idle is byte-conformant** — `urn:xmpp:idle:1`, `since`
   xs:dateTime; idle is orthogonal to the Show and may ride any presence.
3. **The roster in-call overlay is byte-conformant XEP-0108** — namespace
   `http://jabber.org/protocol/activity`, the official `<talking>` /
   `<on_the_phone>` / `<on_video_phone>` values, published over PEP
   (XEP-0163). Waddle uses the XEP; it does not fork it.
4. **`urn:waddle:in-call:0` carries only call-internal substate**
   (`<hand-raised/>`, `<muted/>`) for which no XEP shape exists. It is
   never used for the roster-facing "in a call" signal that XEP-0108
   already covers, and never squats an official namespace.
5. **`urn:waddle:status-preference:0` is a Waddle-custom preference node,
   not an official namespace.** It mirrors the established
   `urn:waddle:dnd:0` transport conventions: owner-only PEP, single item
   id `current`, `access_model=whitelist`, publisher-must-equal-owner.
   It stores the user's chosen Show, which is a Waddle UX preference, not
   a wire presence shape — no XEP defines a "synced chosen presence."

### Sub-decisions

- **Ghost-call cleanup.** The XEP-0108 activity item is account-global and
  durable, so it must be actively cleared. Belt-and-suspenders: explicit
  PEP retract on call-leave **and** on graceful disconnect; for hard
  crashes, the *receiving* client clears a contact's in-call overlay
  locally when that contact's presence goes `unavailable`. No ghost
  "in a call" survives.
- **Activity-node ownership.** In-call is the **sole publisher** of the
  XEP-0108 activity node for now (there is no user-activity-setting UI).
  If manual User Activity ever ships, it must save/restore the prior
  activity around in-call. Documented constraint.
- **Audio vs video** is carried on the wire (`on_the_phone` vs
  `on_video_phone`); the UI decides whether to differentiate the icon.
- **In-call pauses auto-away.** While the overlay is active, the idle
  timer does not downgrade the Effective Show — being in a call is proof
  of presence. Prevents broadcasting "Away + in a call."
- **Most-available ordering.** Do Not Disturb > Available > Away > Extended
  Away > Offline. A deliberate Do Not Disturb wins over every other online
  state, so an explicit "do not disturb" is always shown even when another
  resource is available — it is never masked. (Once cross-device sync lands
  in Phase 4 a manual DND is on every resource anyway; until then this rule
  also resolves the per-device case the same way.)
- **Manual Available is a pin.** Selecting Available is a sticky Manual
  status that defeats the idle timer; "Reset to automatic" is the only
  way back to Automatic mode. The picker must make Available (pinned) and
  Automatic (timer-governed) legible — e.g. only surface "Reset to
  automatic" while a Manual status is active.
- **Presence-DND may suppress own notifications.** When the Effective
  Show is `dnd`, the client may silence its own banners/sounds (in-scope
  extra). This is orthogonal to the server-side `urn:waddle:dnd:0`
  quiet-hours gating; both can suppress, for different reasons.

## Consequences

- **An outbound presence-publish path must be built in the client** — it
  does not exist today, and it is the enabling primitive for every state
  here. This is Phase 1's spine.
- The client gains an **XEP-0108 publish** path (it currently only reads
  activity) and an **XEP-0223 `status-preference`** publish/subscribe +
  own-resource apply path.
- Each Waddle-custom namespace requires a dedicated Rust test suite
  (CLAUDE.md XEP custom test-suite hard rule): the new
  `urn:waddle:status-preference:0`, and any expansion of
  `urn:waddle:in-call:0`.
- The `:0` version suffix marks both custom nodes as Waddle-experimental;
  if the XSF later standardizes a synced-presence or call-presence
  carrier, migration is a bounded re-key.

### Phasing (this is an epic, not a PR)

1. **Manual presence + render.** Outbound presence publish; manual picker
   (Automatic / Available / Away / Do Not Disturb / Reset to automatic);
   most-available-wins on the receiving side. Lights up the states that
   already render.
2. **Auto-away.** Per-device input-timer + XEP-0319 idle stamp; manual
   suspends it.
3. **In-call overlay.** XEP-0108 publish on call lifecycle (1:1 + MUC);
   receiver render; ghost cleanup; in-call pauses auto-away.
4. **Cross-device sync.** `urn:waddle:status-preference:0` (XEP-0223);
   resources adopt and persist the Manual status.
5. **DND quiets own notifications; Chat surfaced in the picker.**

### Out of scope (committed, design not yet done)

- **Custom status** (emoji + text + expiry) — needs its own design pass to
  pick a carrier (`<status>` text + a custom/XEP-0107-style emoji+expiry
  shape). A separate ADR.
- **Out-of-office** (away message + auto-reply) — a separate epic; needs
  message storage and an auto-reply path.
- The DND **schedule** (`urn:waddle:dnd:0`, quiet hours) is unchanged.
