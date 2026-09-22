# Room presence during reconnect

Fresh resource binding and successful stream resumption have different room
presence contracts. Reusing the same full JID does not make a fresh connection
an XEP-0198 continuation of its predecessor.

## Fresh binding

A replacement connection receives a new occupancy generation. Terminal cleanup
still removes the predecessor's generation from every room it occupied, even
when a newer connection already owns the full JID's routing entry. Binding alone
does not subscribe the replacement to any room.

When the replacement explicitly rejoins a room under the same nickname, two
orderings are possible:

| Completed operation order | What other occupants observe |
| --- | --- |
| Old-generation cleanup, then replacement join | Unavailable presence from the old room/nick, followed by available presence from the replacement room/nick. |
| Replacement join, then old-generation cleanup | The existing full-JID occupancy adopts the new generation without a duplicate join announcement. Cleanup rejects the stale generation and emits no departure. |

This is a deliberate behavior trade-off of the terminal-cleanup fix in
[#1821](https://github.com/waddle-social/waddle/pull/1821). Generation fencing
protects an already seated replacement; it cannot suppress a departure that
completed before the replacement joined. Restoring the broad superseded-session
gate would again leak rooms that the replacement never rejoins. The fresh-bind
path offers no guarantee of seamless room presence. Fresh binding after an
already completed resumable detach also removed the old occupancy before that
PR; the newly explicit trade-off concerns replacement of an active connection.

The unavailable stanza uses the ordinary
[XEP-0045 §7.14](https://xmpp.org/extensions/xep-0045.html#exit) shape, including
`muc#user/item role='none'`. A later join follows
[XEP-0045 §7.2](https://xmpp.org/extensions/xep-0045.html#enter). No new namespace,
status code, or custom reconnect signal is introduced.

## Successful stream resumption

A resumable transport loss retains the room occupancy while the detached
session remains valid. An authenticated
[XEP-0198 §5](https://xmpp.org/extensions/xep-0198.html#resumption) resume carries
forward the original occupancy generation and handled-stanza counts. It does
not fresh-bind a resource or require a MUC rejoin. Room messages continue to
reach the resumed stream without announcing a departure.

If the client abandons resumption and fresh-binds the same resource instead,
the detached predecessor is invalidated and its rooms are cleaned. Expired or
failed resumption must not be treated as successful continuity.

## Client impact

The web client preserves SM resume state and supplies it when reconnecting
(`chat/src/lib/xmpp/client.ts` and `resume-persistence.ts`). Native applications
currently construct a new Rust client through
`server/crates/waddle-xmpp-client-ffi/src/lib.rs` without supplying saved SM state;
those reconnects use fresh binding. Saving message-history cursors is separate
from saving a resumable XMPP stream.

A real unavailable/available pair updates occupant rosters and presence-derived
call participant state. Clients must accept both sequences. There is no promise
that a fresh mobile reconnect is visually seamless: a participant may disappear
and reappear, and call presence must be advertised again by the new generation.
Ordinary room presence is not itself a chat message; it must not fabricate
"left/joined" timeline entries or restore old call state from a plain available
presence. The client regression tests exercise these event projections; they
do not measure rendered flicker or device-network timing.

## Regression coverage

- `websocket/tests/xep0045_reconnect_contract.rs` awaits the real join and
  terminal-cleanup paths in both orders. It checks typed outbound presence,
  generation ownership, and the surviving replacement's explicit departure.
  No sleeps or production scheduling controls choose the ordering.
- `tests/xep0045_join_lifecycle_ws.rs` checks that fresh binding invalidates a
  detached occupancy and does not inherit fan-out from a room it never joined.
- `tests/xep0198_muc_resume_ws.rs` resumes an actual WebSocket stream, exchanges
  room messages without rejoining, checks the observer's traffic for departures,
  and inspects a second detached snapshot to prove the resumed connection kept
  its predecessor's occupancy generation.

These contracts complement [#1733](https://github.com/waddle-social/waddle/issues/1733)
and the cleanup work in [#1811](https://github.com/waddle-social/waddle/issues/1811).
The remaining durability responsibilities have dedicated follow-ups:

- [#1825](https://github.com/waddle-social/waddle/issues/1825): recover remote
  membership cleanup after a process restart.
- [#1826](https://github.com/waddle-social/waddle/issues/1826): retain cleanup
  responsibility across the remote join acknowledgement gap.

The reconnect contract does not close those durability gaps.
