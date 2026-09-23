# Keep channels out of direct chats

Opening Chat must keep the DM selection when an earlier room-list request
finishes. A normal channel must not appear in the DM message pane or DM list.

## Plan

- Reproduce the late room-list response and early room private-message cases.
- Keep room discovery from restoring a selection after a newer navigation.
- Allow only direct chats and confirmed group DMs in the DM message pane.
- Use the existing occupant-JID classifier for live room private messages.
- Preserve the XEP-0045 private-message marker through Rust parsing, browser
  bindings, and history so custom MUC services work before discovery.
- Reject direct chat bodies addressed to a bare MUC room before the server
  writes direct-message history or inbox state. Keep valid room control flows.
- Remove false room-bare DM entries when room identity becomes known, including
  entries restored from browser storage. Keep valid occupant and account chats.
- Add regression tests for response order, saved-state cleanup, and group DMs.
- Run the chat tests, lint, build checks, and independent adversarial reviews.
- Update the PR, mark it ready, and fix any failed CI checks.

## Protocol constraints

XEP-0045 section 7.5 identifies a private-message peer by its full occupant
JID. Keep that identity for messages received before room discovery completes.
XEP-0280 can deliver these messages from another session. Use standard XMPP
messages and errors. Do not introduce an API outside XMPP or classify rooms
by display name. A missing private-message marker must still work when the
configured service, discovery, or saved occupant scope identifies the room.

## Recheck

The three isolated reproductions still fail on the code used by main on
2026-09-24. The latest fetched commits change server and Apple code; the
affected chat files are unchanged. The previous fix remains present.

The second review found that selecting a saved DM must also cancel the old
route request. A presentation guard alone cannot fix this race.

The server review found another source of false entries: a direct chat to a
bare room address could reach the account inbox write before delivery failed.
The client parser also discarded the standard private-message marker. These
paths need fixes at their source, in addition to selection control and cleanup
of saved entries.
