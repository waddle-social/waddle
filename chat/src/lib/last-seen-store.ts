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

function storage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function getLastSeen(key: string): string | null {
  const s = storage();
  if (!s) return null;
  try {
    return s.getItem(key);
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
