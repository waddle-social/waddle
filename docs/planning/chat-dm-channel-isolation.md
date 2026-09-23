# Keep channels out of direct chats

Opening Chat must keep the DM selection when an earlier room-list request
finishes. A normal channel must not appear in the DM message pane or DM list.

## Plan

- Reproduce the late room-list response and early room private-message cases.
- Keep room discovery from restoring a selection after a newer navigation.
- Allow only direct chats and confirmed group DMs in the DM message pane.
- Use the existing occupant-JID classifier for live room private messages.
- Remove false room-bare DM entries when room identity becomes known, including
  entries restored from browser storage. Keep valid occupant and account chats.
- Add regression tests for response order, saved-state cleanup, and group DMs.
- Run the chat tests, lint, build checks, and independent adversarial reviews.
- Update the PR, mark it ready, and fix any failed CI checks.

## Protocol constraints

XEP-0045 section 7.5 identifies a private-message peer by its full occupant
JID. Keep that identity for messages received before room discovery completes.
XEP-0280 can deliver these messages from another session. Do not change the
wire format, introduce an API outside XMPP, or classify rooms by display name.

## Recheck

The three isolated reproductions still fail on the code used by main on
2026-09-24. The latest fetched commits change server and Apple code; the
affected chat files are unchanged. The previous fix remains present.
