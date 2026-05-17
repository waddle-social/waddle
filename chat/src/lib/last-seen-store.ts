// Per-conversation "last message the local user has viewed" anchor, stored in
// localStorage so the client can restore scroll position between sessions and
// render a "new messages" divider. This is a local UX concern distinct from
// XEP-0333 <displayed/> receipts, which communicate *other users'* read state.

const PREFIX = "waddle.chat.last-seen";

export function roomKey(channelId: string): string {
  return `${PREFIX}.room.${channelId}`;
}

export function dmKey(peerBareJid: string): string {
  return `${PREFIX}.dm.${peerBareJid}`;
}

/**
 * Key used by XEP-0490 MDS to persist the latest displayed stanza-id
 * received from another device of the same account. The chat id is
 * the bare JID of the chat (DM contact or MUC room) per XEP-0490 §3.
 * The local readers that own the "new messages" divider for the
 * corresponding conversation can look up this key in addition to
 * their conversation-scoped key.
 */
export function mdsChatKey(chatBareJid: string): string {
  return `${PREFIX}.mds.${chatBareJid}`;
}

function storage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function setLastSeen(key: string, messageId: string): void {
  const s = storage();
  if (!s) return;
  try {
    s.setItem(key, messageId);
  } catch {
    // Quota exceeded or storage disabled; silently drop — the worst outcome is
    // that the divider shows up on the next visit instead of not at all.
  }
}
