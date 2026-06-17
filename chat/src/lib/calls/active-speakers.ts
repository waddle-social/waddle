import type { CallTileModel } from "./call-tiles";

/**
 * Default brief hold for the active-speaker highlight. LiveKit already
 * debounces its `ActiveSpeakersChanged` signal, but the highlight is held a
 * little longer on top so it does not flicker through the natural gaps between
 * words once a participant stops talking. ~1s reads like Zoom/Meet.
 */
export const ACTIVE_SPEAKER_HOLD_MS = 1_000;

export type ActiveSpeakerState = {
  /** Identities LiveKit reported as speaking as of the last step. */
  readonly speaking: ReadonlySet<string>;
  /**
   * Identities that have stopped speaking but are still highlighted, mapped to
   * the wall-clock ms after which the highlight is released.
   */
  readonly releasing: ReadonlyMap<string, number>;
};

export function emptyActiveSpeakerState(): ActiveSpeakerState {
  return { speaking: new Set(), releasing: new Map() };
}

export type AdvanceActiveSpeakersInput = {
  state: ActiveSpeakerState;
  /** Identities LiveKit currently reports as actively speaking. */
  speakingIdentities: readonly string[];
  /** Wall-clock time of this step, in ms (injected so the hold is testable). */
  now: number;
  /** How long the highlight is held after a participant stops speaking. */
  holdMs?: number;
};

export type ActiveSpeakerStep = {
  state: ActiveSpeakerState;
  /** Every identity currently highlighted (speaking or within the hold). */
  activeIdentities: ReadonlySet<string>;
  /**
   * Earliest wall-clock ms at which a held highlight will next need releasing,
   * or `null` when nothing is counting down. Drives a single scheduled sweep so
   * the highlight clears even with no further LiveKit events.
   */
  nextDeadline: number | null;
};

export function advanceActiveSpeakers(input: AdvanceActiveSpeakersInput): ActiveSpeakerStep {
  const holdMs = input.holdMs ?? ACTIVE_SPEAKER_HOLD_MS;
  const speaking = new Set<string>();
  for (const identity of input.speakingIdentities) {
    const trimmed = identity.trim();
    if (trimmed) speaking.add(trimmed);
  }

  const releasing = new Map<string, number>();
  // A participant who was speaking but no longer is starts the hold now.
  for (const identity of input.state.speaking) {
    if (!speaking.has(identity)) releasing.set(identity, input.now + holdMs);
  }
  // Participants already counting down keep their deadline until it elapses; a
  // resumed speaker is dropped here and re-added via `speaking`.
  for (const [identity, deadline] of input.state.releasing) {
    if (speaking.has(identity) || releasing.has(identity)) continue;
    if (deadline > input.now) releasing.set(identity, deadline);
  }

  const activeIdentities = new Set<string>(speaking);
  let nextDeadline: number | null = null;
  for (const [identity, deadline] of releasing) {
    activeIdentities.add(identity);
    nextDeadline = nextDeadline === null ? deadline : Math.min(nextDeadline, deadline);
  }

  return {
    state: { speaking, releasing },
    activeIdentities,
    nextDeadline,
  };
}

/**
 * Drop a participant from the active-speaker hold entirely — both the live
 * speaking set and any in-flight release countdown. Used when a participant
 * leaves the call so their brief highlight (and any Speaker-view promotion
 * derived from it) cannot survive their departure and re-apply the instant they
 * rejoin with a fresh tile. Returns the same state when the identity is absent,
 * so a departure that touches no speaker state causes no needless churn.
 */
export function evictActiveSpeaker(
  state: ActiveSpeakerState,
  identity: string,
): ActiveSpeakerState {
  if (!state.speaking.has(identity) && !state.releasing.has(identity)) return state;
  const speaking = new Set(state.speaking);
  speaking.delete(identity);
  const releasing = new Map(state.releasing);
  releasing.delete(identity);
  return { speaking, releasing };
}

export function highlightedTileKeys(
  tiles: readonly CallTileModel[],
  activeIdentities: ReadonlySet<string>,
): ReadonlySet<string> {
  const keys = new Set<string>();
  for (const tile of tiles) {
    // The speaker is the person, not their screen — only their camera tile
    // carries the highlight.
    if (tile.source !== "camera") continue;
    if (activeIdentities.has(tile.identity)) keys.add(tile.key);
  }
  return keys;
}
