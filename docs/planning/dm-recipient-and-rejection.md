# DM recipient identity and message rejection

New DM currently constructs an account JID from unchecked text. Room discovery
can later replace that account destination with a room whose localpart matches.
Message errors can enter the normal incoming-message path, and a stream
acknowledgement can prevent a later rejection from marking the send as failed.

## Plan

1. Select canonical local account JIDs through the existing XMPP user directory.
   Include native and OIDC accounts in the directory. Keep lookup failures visible.
2. Preserve account, room, and occupant identities in navigation. Remove localpart
   substitution and name-based conversation cleanup. Test delayed room discovery
   and an account with the same name as a room.
3. Parse message errors into typed rejection events before normal message effects.
   Pass them through Rust, WASM, and the browser; correlate them with the outbound
   message and recipient. Do not add incoming content or unread state.
4. Make explicit rejection override stream acceptance without changing the rule
   that a transport failure cannot undo a confirmed acknowledgement. Remove
   rejected messages from automatic retry and display the failed send.
5. Fail account lookup errors with a temporary stanza error before recipient
   storage effects. Preserve existing offline delivery and durable rejection.
6. Add focused regression and conformance tests, run chat and Rust checks, and
   obtain independent adversarial reviews of identity, protocol, and durability.
   Complete the PR and monitor CI until all checks pass.

## Protocol constraints

The plan uses XEP-0055 search, XEP-0045 room and occupant identities, XEP-0280
carbons, and XEP-0198 acknowledgement semantics from the local `xeps` sources.
RFC 6121 permits service-unavailable for a nonexistent account; presence
subscription is not proof that an account exists. A stream acknowledgement is
not proof of delivery to the recipient. A shared localpart does not make an
account JID and a room JID interchangeable. No custom wire namespace or non-XMPP
account API is required.
