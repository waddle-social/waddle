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
   Pass them through Rust, WASM, FFI, and each client; correlate them with the outbound
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

## Result

- New DM searches the local XMPP directory and requires an account selection.
  Search covers both account stores and ranks exact addresses before its limit.
  A room and an account can share a localpart without changing the destination.
- DM routes and notification links carry the complete peer JID. Room discovery
  cannot replace an account selection. Occupant links carry explicit scope,
  so failed or partial discovery cannot remove the resource. Selected occupant
  context also keeps incoming replies on that exact peer.
- Message errors become typed Rust rejection events. The WASM callback carries
  the stanza ID, sender, recipient, and error metadata. The native FFI bridge
  carries validated ID/JID values to native rejection handlers. Errors cannot trigger
  incoming content, calls, reactions, or unread-message effects.
- The browser checks the outbound ID and recipient before applying rejection.
  Trusted sent carbons provide the same evidence for another device. Rejection
  survives later stream acknowledgements and sent-carbon reconciliation.
- Native clients correlate the same typed rejection with their outbound sends.
  Android stores rejection on the shared timeline, so retry, screen recreation,
  and message merging cannot restore a rejected echo as a successful message.
- Rejected sends leave the browser retry queue. Native stream state retains
  their sequence positions and rejection flags across persistence, excludes
  them from fresh-stream retries, and does not report them as delivered.
- Account lookup failure returns `wait/internal-server-error`. Routing tests
  assert no MAM, inbox, or recipient-delivery writes; replay keeps the recorded
  denial after the database recovers.

## Validation

Regression coverage includes the complete delayed-discovery DM flow, nonexistent
account selection, full-address input, occupant and external DM routes, lookup
failure, both acknowledgement/rejection orders, immediate rejection, forged
sender correlation, sent carbons, archive errors, and restored retry queues.
Dedicated Rust suites cover XEP-0055, XEP-0045, XEP-0198, XEP-0280, and XEP-0313.
Chat tests, Knip, the production build, Rust tests, and strict Clippy checks are
required before the final push. Independent reviews cover recipient identity,
protocol boundaries, and server storage effects.
