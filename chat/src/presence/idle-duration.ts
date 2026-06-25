// Receiving-side render helper: turn an XEP-0319 `<idle since>` instant into a
// short human label ("idle 20m") shown next to a contact's away presence.
// Pure so the render is testable without a clock.

/**
 * Format the idle age of a contact whose presence carried `<idle since>`.
 * `sinceMs` and `nowMs` are epoch ms. Coarse-grained on purpose — minutes,
 * then hours, then days — matching how long-idle is actually useful to read.
 * Clamps non-positive gaps (clock skew / "since" in the future) to "<1m".
 */
export function formatIdle(sinceMs: number, nowMs: number): string {
  const ms = Math.max(0, nowMs - sinceMs);
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 1) return "idle <1m";
  if (minutes < 60) return `idle ${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `idle ${hours}h`;
  return `idle ${Math.floor(hours / 24)}d`;
}
