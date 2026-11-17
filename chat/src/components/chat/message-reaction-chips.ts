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
