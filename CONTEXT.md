# Waddle — Domain Glossary

Terms with a precise meaning in this codebase. Keep entries
implementation-free: what a thing *is*, not how it's built.

## Calls

### Active Call
The single call the local user has currently joined. A user is in at
most one Active Call at a time. Being in an Active Call is independent
of which conversation they are viewing.

### Originating Conversation
The conversation a call belongs to: the channel (MUC room) where the
call was started, or the DM pair for a 1:1 call. A call has exactly one
Originating Conversation for its lifetime.

### Session-bound audio (rule)
An Active Call's audio follows the user across the entire app. The user
hears the call regardless of which conversation, dashboard, or surface
they are viewing, until they explicitly leave the call. Losing audio is
never a side effect of navigation.

### Membership-scoped visibility (rule)
A call is visible and joinable only to members of its Originating
Conversation. For a channel call, joining requires being a *current
occupant* of the channel (present in the room at the moment of
joining); for a DM call, only the two participants may join.
Non-members must not be able to see that the call exists or obtain the
means to join it. This must hold at the protocol level, not merely in
what the UI shows.

If a participant's membership ends involuntarily (kick or ban), their
call participation ends with it. Transient connection loss is not a
membership change and must never end call participation.

### Call Surface
A visible UI region rendering an ongoing call (participant tiles,
controls). Call Surfaces are bound to the Originating Conversation's
view and may come and go with navigation — unlike audio, which is
session-bound.
