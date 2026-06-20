# Waddle

Glossary of domain terms used across the Waddle XMPP server and chat client. This file is a shared language reference, not a spec — it defines what terms *mean*, not how features are built.

## Presence

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
Online but does not want to be interrupted. Busy.
_Avoid_: Busy, offline-to-others.

**Chat**:
Online and actively interested in conversing ("free for chat").
_Avoid_: Active.

**Offline**:
Not connected / no available resource. Sent as an `unavailable` presence.
_Avoid_: Invisible (invisibility is a separate, unimplemented concept).

**Idle**:
How long since the user last interacted, stamped on a presence via XEP-0319 (`urn:xmpp:idle:1`). Orthogonal to the Show — an Away presence may also carry an idle timestamp so others can render "away, idle 20m."
_Avoid_: Last seen (that is the offline XEP-0012 concept).

**Manual status**:
A Show the local user deliberately selected. Overrides auto-away.
_Avoid_: Custom presence.

**Auto-away**:
A Show the client sets on the user's behalf after detected inactivity (Available → Away → Extended Away), stamped with an idle timestamp.
_Avoid_: Auto-idle.
