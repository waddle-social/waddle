# ADR-010: Session-Scoped Call Audio via a Single Persistent Sink

## Status

Accepted

## Date

2026-06-11

## Context

Joining a channel call and then navigating to any other view silenced
the call: the user stayed connected (the LiveKit room lives in a
process-wide singleton engine, and the mic kept publishing) but could
no longer hear anyone.

Root cause: the only `<audio>` elements playing remote call audio
lived inside `CallTile.vue`, mounted by the per-conversation call
surfaces (`CallSplitContainer` / `CallExpandedSurface`). Those
surfaces render only when the *viewed* conversation is the call's
Originating Conversation (`ownsMucCall` gating), and the whole page
island unmounts on navigation (Astro view transitions persist only
`XmppProvider` / `AppShell`). On unmount, the `:ref` callbacks detach
the tracks and clear `srcObject` — by design, to keep LiveKit's
`attachedElements` clean. Audio playback was therefore route-scoped
while the call connection was session-scoped.

History note: `CallOverlay.vue` once owned the entire call UI and was
already persistent. The refactor that split presentation into
per-channel surfaces moved audio playback with it and introduced this
bug — evidence that the invariant is easy to break silently when it is
not recorded.

### Options considered

- **(A) Tile-owned audio (status quo).** Audio elements live in
  participant tiles. Rejected: ties hearing a call to viewing it;
  violates the Session-bound audio rule (CONTEXT.md).
- **(B) Dual sinks with handoff.** Tiles keep audio while a surface is
  mounted; a global sink takes over on unmount. Rejected: every
  navigation needs detach-here/attach-there choreography; any bug in
  it yields silence (this bug again) or double-attach echo — the exact
  failure class `tile-attach.ts` exists to prevent.
- **(C) Single persistent sink.** One headless component in the
  persistent layer owns all remote audio playback for the Active
  Call's lifetime; tiles render video only. **Chosen.**

## Decision

Adopt **(C)**. Call audio playback is **session-scoped**: a headless
`CallAudioSink` mounted in the persistent layer (`XmppProvider`, which
carries `transition:persist`, next to `CallOverlay`) owns hidden
`<audio>` elements for **all** subscribed remote audio tracks
(screen-share audio included), reusing the `TileAttachments`
reconciler. `CallTile` renders video and nameplate only.

**Invariant: anything required to keep Session-bound audio true must
live in the persistent layer, never in a route-scoped surface.** This
covers both the audio sink and the means to unblock it — the
autoplay-blocked recovery prompt (`CallAudioPlaybackPrompt`, which
drives `room.startAudio()`) is likewise mounted once in the persistent
layer and removed from the call surfaces.

Per-participant volume is unaffected: the mixer applies
`track.setVolume()`, which is element-agnostic.

## Consequences

- Users keep hearing an Active Call across every view; losing audio is
  never a side effect of navigation (Session-bound audio rule,
  CONTEXT.md).
- Future contributors must not "simplify" audio back into participant
  tiles, even though upstream LiveKit examples attach audio there —
  that reintroduces this bug. This ADR is the guard.
- Call surfaces become pure decoration: they may mount and unmount
  freely without touching audio machinery.
- Companion protocol rule (not this ADR's subject): call joinability is
  occupancy-gated server-side at LiveKit token mint, and involuntary
  MUC removal evicts the participant from the SFU call (#935).
