// The outbound "brain": resolves the user's presence mode into the Show
// to broadcast. Automatic mode lets the idle timer govern; a Manual pick
// overrides it. See docs/adr/010-presence-statuses.md.

import type { AutoAway } from "./idle";

export type ManualStatus = "available" | "away" | "dnd";

export type EffectiveShow = "available" | "away" | "dnd";

export type PresenceMode =
  | { kind: "automatic" }
  | { kind: "manual"; status: ManualStatus };

export function resolveShow(mode: PresenceMode): EffectiveShow {
  return mode.kind === "manual" ? mode.status : "available";
}

/** The Show this device actually broadcasts. Adds `xa` over the manual set —
 * Extended Away is only ever reached automatically, never picked. */
export type BroadcastShow = "available" | "away" | "xa" | "dnd";

/** The full outbound presence this device should advertise: a Show plus the
 * XEP-0319 idle instant (epoch ms) to stamp, or `null` for none. */
export interface BroadcastPresence {
  show: BroadcastShow;
  idleSince: number | null;
}

/**
 * Resolve the Effective Show (ADR-010 precedence): a Manual status wins and
 * suspends auto-away (so it never carries an idle stamp); Automatic mode
 * adopts the per-device auto-away computation, idle stamp and all. This is the
 * single authority for what to put on the wire — used by the live idle tracker
 * and by the reconnect re-broadcast so a fresh session restores the current
 * away+idle rather than a stale Available.
 */
export function resolveBroadcast(mode: PresenceMode, autoAway: AutoAway): BroadcastPresence {
  if (mode.kind === "manual") return { show: mode.status, idleSince: null };
  return { show: autoAway.show, idleSince: autoAway.idleSince };
}

export type PresencePick = ManualStatus | "reset";

/** Map a picker choice to the next mode: a status pins manual, `reset` returns to Automatic. */
export function applyPick(pick: PresencePick): PresenceMode {
  return pick === "reset" ? { kind: "automatic" } : { kind: "manual", status: pick };
}
