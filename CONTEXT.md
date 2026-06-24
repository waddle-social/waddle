# Waddle

Glossary of domain terms used across the Waddle XMPP server and chat client. This file is a shared language reference, not a spec — it defines what terms *mean*, not how features are built.

## Presence

### Availability states

**Presence**:
A user's network availability, broadcast to roster subscribers and reflected into joined rooms. Carries an availability state (the `<show>`), optional free-text status, and a priority. Distinct from *rich status* (mood/activity/tune), which is published separately via PEP.
_Avoid_: Online/offline (that is only two of the states).

**Show**:
The XMPP availability state — the `<show>` value of a presence stanza. One of: *Available*, *Away*, *Extended Away*, *Do Not Disturb*, *Chat*. Absence of a show element means plain Available; an `unavailable` presence means Offline.
_Avoid_: Status (reserve "status" for the free-text line).

**Available**:
Online and reachable, no qualifier. The default state when connected.
_Avoid_: Online (online includes Away/Extended Away/DND too).

**Away**:
Online but temporarily not at the keyboard — a short, "stepped away" absence.
_Avoid_: Idle, AFK.

**Extended Away** (`xa`):
Online but away from the computer for a longer period. This is the state meant by "online but away from computer."
_Avoid_: Long away, gone.

**Do Not Disturb** (`dnd`):
The `<show>dnd</show>` availability state — online but does not want to be interrupted, broadcast to contacts. Distinct from the *DND schedule* (`urn:waddle:dnd:0`), which silences push notifications on a timetable; this term is only ever the Show.
_Avoid_: Busy, offline-to-others; and do not conflate with the DND schedule.

**Chat**:
Online and actively interested in conversing ("free for chat").
_Avoid_: Active.

**Offline**:
Not connected / no available resource. Sent as an `unavailable` presence.
_Avoid_: Invisible (invisibility is a separate, unimplemented concept).

### Idle & auto-away

**Idle**:
How long since the user last interacted, stamped on a presence via XEP-0319 (`urn:xmpp:idle:1`). Orthogonal to the Show — an Away presence may also carry an idle timestamp so others can render "away, idle 20m."
_Avoid_: Last seen (that is the offline XEP-0012 concept).

**Auto-away**:
A Show the client sets on the user's behalf from a per-device inactivity timer (Available → Away after ~10 min without interaction or with the tab hidden → Extended Away after ~30 min), stamped with an idle timestamp. Browser clients approximate idle from in-page input and tab visibility only; they cannot see OS-level idle. Auto-away is never synced across devices.
_Avoid_: Auto-idle.

### Manual control & cross-device sync

**Automatic**:
The default presence mode: the per-device idle timer governs the Show. The user has not pinned a Manual status.
_Avoid_: Default presence.

**Manual status** (Manual override):
A Show the user deliberately selected — Available, Away, or Do Not Disturb. Sticky: it suspends Auto-away on that account until the user resets.
_Avoid_: Custom presence.

**Pinned Available**:
A Manual status of Available — keeps the user shown as Available against the idle timer. Distinct from Automatic, where the timer would eventually mark them Away.
_Avoid_: Force online.

**Reset to automatic**:
The action that clears a Manual status and returns the client to Automatic mode.
_Avoid_: Clear presence.

**Status preference**:
The user's current Manual status, stored account-wide in the `urn:waddle:status-preference:0` PEP node (XEP-0223 persistent private storage) so all of the user's resources adopt it and it survives reconnects. Auto-away is *not* part of the preference — it stays per-device.
_Avoid_: Saved presence.

**Effective Show**:
The single Show a given resource actually broadcasts, after resolving precedence on that device: Manual status (if set) over the Auto-away computation. The In-call overlay never changes the Effective Show.
_Avoid_: Computed presence.

**Most-available-wins**:
The rule a *receiving* client uses to collapse a contact's several resources into one rendered Show: the most available resource wins (Available > Away > Extended Away > Offline). A Do Not Disturb set as a synced Manual status appears on every resource, so it is never masked by a more-available device.
_Avoid_: Highest priority (priority is the routing concept, not the render rule).

### In a call

**In-call overlay**:
An orthogonal signal that a user is in a live call, layered *on top of* their Show and never replacing it — a user can be "Available + in a call" or "DND + in a call." Derived automatically from joining a call (1:1 or MUC) and retracted on leave. Pauses Auto-away while active.
_Avoid_: In-call status, Busy (it is not a Show).

**In-call activity** (roster-facing):
The XEP-0108 User Activity publication — `<activity><talking><on_the_phone/></talking>` or `<on_video_phone/>` — that tells roster contacts (outside the call room) that the user is in a call.
_Avoid_: Conflating with the In-call substate below.

**In-call substate** (in-room):
The `urn:waddle:in-call:0` presence markers (`<hand-raised/>`, `<muted/>`) visible to fellow occupants of a call room. Distinct from the roster-facing In-call activity; it carries call-internal state that no XEP models.
_Avoid_: Calling it presence-DND or an availability state.

### Notification suppression

**DND schedule** (Quiet hours):
The user's push-notification suppression policy — a one-shot snooze plus weekly quiet-hours rules — stored in the `urn:waddle:dnd:0` PEP node and evaluated server-side. Distinct from the **Do Not Disturb** Show: the schedule silences pushes on a timetable; the Show tells contacts not to disturb you right now.
_Avoid_: Calling either one just "DND" without qualifying which.

### Deferred (committed scope, design not yet done)

**Custom status**:
A free-text status line with an emoji and optional expiry (e.g. "🌴 On vacation, back Monday"). Independent of the Show. Wire carrier not yet decided.
_Avoid_: Mood (mood is the XEP-0107 concept).

**Out-of-office**:
An away-message / auto-reply state (Teams-style), separate from availability. Needs message storage and an auto-reply path.
_Avoid_: Extended Away (that is only a Show, with no message).
