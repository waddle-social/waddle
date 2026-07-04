/* Warmth tier — count-proportional ambient halo on the reaction chip.
 * Replaces the "metric chip" feel with an "ambient acknowledgement"
 * read: at a glance the room shows you which posts resonated, not
 * just which ones got a tap. Thresholds are intentionally low so a
 * 3-person room still gets the warmest tier on a single shared
 * reaction. */
export function reactionWarmthClass(count: number): string {
  if (count >= 5) return "chat-reaction-chip--warmth-hot";
  if (count >= 3) return "chat-reaction-chip--warmth-warm";
  if (count >= 2) return "chat-reaction-chip--warmth-tepid";
  return "";
}

const reactionListFormatter = new Intl.ListFormat(undefined, { style: "long", type: "conjunction" });

export function formatReactors(nicks: readonly string[]): string {
  return reactionListFormatter.format([...nicks]);
}

export function reactionAriaLabel(emoji: string, nicks: readonly string[]): string {
  return `${formatReactors(nicks)} reacted with ${emoji}`;
}

export function isReactor(nicks: readonly string[], me: string | undefined): boolean {
  if (!me) return false;
  for (const nick of nicks) {
    if (nick === me) return true;
  }
  return false;
}
