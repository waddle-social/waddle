const BROADCAST_MENTION_SET = new Set(["everyone", "here"]);

function canonicalMentionIdentifier(value: string): string {
  return value.trim().replace(/^xmpp:/i, "").replace(/^@+/, "").toLowerCase();
}

export function isBroadcastMention(value: string): boolean {
  return BROADCAST_MENTION_SET.has(canonicalMentionIdentifier(value));
}

export function mentionMatchesUsername(mention: string, username?: string | null): boolean {
  if (!username) return false;
  return canonicalMentionIdentifier(mention).split("@")[0] === canonicalMentionIdentifier(username);
}

export function resolveMentionUri(
  mention: string,
  mentionJidsByNick?: Readonly<Record<string, string>>,
): string {
  const canonicalMention = canonicalMentionIdentifier(mention);
  const mappedBareJid = mentionJidsByNick
    ? Object.entries(mentionJidsByNick).find(([nick]) =>
        canonicalMentionIdentifier(nick) === canonicalMention
      )?.[1]
    : undefined;

  return `xmpp:${canonicalMentionIdentifier(mappedBareJid ?? canonicalMention)}`;
}
