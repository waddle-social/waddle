// The auto-away "brain": a pure function that maps how long this device has
// been inactive into the Show to broadcast, plus the XEP-0319 idle-since
// instant. Automatic mode (ADR-010) consults this; a manual pick suspends it.

/** The Show auto-away can derive. Never DND — that is only ever a manual pick. */
type AutoAwayShow = "available" | "away" | "xa";

export interface AutoAwayThresholds {
  /** Inactivity before Away (`<show>away</show>`). */
  awayMs: number;
  /** Inactivity before Extended Away (`<show>xa</show>`); must exceed `awayMs`. */
  xaMs: number;
}

/**
 * Default inactivity thresholds (ADR-010): Away after ~10 min, Extended Away
 * after ~30 min. Tunable — the tracker accepts overrides; these are the
 * shipped defaults.
 */
export const DEFAULT_AUTO_AWAY_THRESHOLDS: AutoAwayThresholds = {
  awayMs: 10 * 60_000,
  xaMs: 30 * 60_000,
};

export interface AutoAway {
  show: AutoAwayShow;
  /**
   * Epoch ms of the instant interaction stopped — the XEP-0319 `<idle since>`
   * value — or `null` while Available (no idle element on the wire).
   */
  idleSince: number | null;
}

/**
 * Resolve the auto-away Show from the inactivity gap. `lastActive` is the last
 * moment the user interacted in-tab (it freezes while the tab is hidden and
 * resets on refocus — the driver owns that); the idle stamp is `lastActive`
 * itself, never `now`, so contacts see when interaction actually stopped.
 */
export function computeAutoAway(
  args: { now: number; lastActive: number } & AutoAwayThresholds,
): AutoAway {
  const { now, lastActive } = args;
  const idleFor = now - lastActive;
  if (idleFor >= args.xaMs) return { show: "xa", idleSince: lastActive };
  if (idleFor >= args.awayMs) return { show: "away", idleSince: lastActive };
  return { show: "available", idleSince: null };
}
