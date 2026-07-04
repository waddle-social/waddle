# ADR-0016: Drop MIX; channels stay on MUC

## Status

Accepted (decision taken 2026-04-19, recorded here after the original
note in `server/TODO.md` was retired).

## Context

Channels needed a group-chat substrate. Two candidates existed: classic
Multi-User Chat (XEP-0045) and Mediated Information eXchange (MIX,
XEP-0369 with its 0403/0404/0405/0406/0407/0408 satellite series).
RFC-0002 originally described channels as "MUC rooms with MIX
extensions."

## Decision

Waddle stays on MUC. MIX (XEP-0369 / 0405 / 0407) was investigated and
dropped.

- The MIX series is inactive: XEP-0403, 0404, 0406, 0407, and 0408 are
  Deferred.
- No major server or client ships it (Conversations, Dino, Gajim,
  Prosody, ejabberd, Openfire all lack production MIX support).
- Community consensus has settled on MUC-with-extensions.

Persistent membership and modern semantics are provided by MUC plus
extension XEPs (bookmarks, MAM, hats, retraction, etc.) rather than MIX.

## Consequences

- RFC-0002's MIX references are superseded; channels are plain MUC
  rooms with extension XEPs layered on.
- No MIX namespaces are advertised or implemented, so no MIX test
  suites are required under the per-XEP test-suite rule.
