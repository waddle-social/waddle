// The outbound "brain": resolves the user's presence mode into the Show
// to broadcast. Automatic mode lets the (future) idle timer govern;
// a Manual pick overrides it. See docs/adr/010-presence-statuses.md.

export type ManualStatus = "available" | "away" | "dnd";

export type EffectiveShow = "available" | "away" | "dnd";

export type PresenceMode =
  | { kind: "automatic" }
  | { kind: "manual"; status: ManualStatus };

export function resolveShow(mode: PresenceMode): EffectiveShow {
  return mode.kind === "manual" ? mode.status : "available";
}

export type PresencePick = ManualStatus | "reset";

/** Map a picker choice to the next mode: a status pins manual, `reset` returns to Automatic. */
export function applyPick(pick: PresencePick): PresenceMode {
  return pick === "reset" ? { kind: "automatic" } : { kind: "manual", status: pick };
}
