/**
 * Per-conversation "last-seen" tracker for XEP-0313 MAM catch-up on reconnect.
 *
 * Why: mobile Safari (and any long-lived WebSocket on a sleeping device)
 * aggressively suspends the socket. When it resumes, any messages delivered
 * during the suspend are already gone from the server's live routing. Without
 * a catch-up step, the UI silently misses them. This helper lets the client:
 *
 *   1. Remember the most recent message timestamp seen for each open
 *      conversation (DM peer bare JID or MUC room bare JID).
 *   2. Distinguish the very first `session:started` (initial login — nothing
 *      to catch up on) from subsequent ones (resume — catch up on all
 *      tracked conversations via `MAM start=lastSeen`).
 *
 * DM peers and rooms are tracked in separate namespaces so the two can never
 * collide even if a bare JID were somehow valid in both worlds.
 *
 * Kept as a tiny pure class so the orchestration in `BrowserXmppClient` has
 * zero state of its own and the logic is trivially testable.
 */
export type CatchupEntry =
  | { kind: "dm"; key: string }
  | { kind: "room"; key: string };

export class ReconnectCatchup {
  private readonly dmLastSeen = new Map<string, string>();
  private readonly roomLastSeen = new Map<string, string>();
  private sessionStartedOnce = false;

  /**
   * Record that a DM with `peerBareJid` was seen at `timestamp`. Only
   * advances the cursor — out-of-order or older arrivals are ignored so the
   * next catch-up can't re-pull already-applied messages.
   */
  recordDmSeen(peerBareJid: string, timestamp: string): void {
    advance(this.dmLastSeen, peerBareJid, timestamp);
  }

  /** As `recordDmSeen`, but for a MUC room JID. */
  recordRoomSeen(roomBareJid: string, timestamp: string): void {
    advance(this.roomLastSeen, roomBareJid, timestamp);
  }

  getDmLastSeen(peerBareJid: string): string | undefined {
    return this.dmLastSeen.get(peerBareJid);
  }

  getRoomLastSeen(roomBareJid: string): string | undefined {
    return this.roomLastSeen.get(roomBareJid);
  }

  /**
   * Notify that `session:started` just fired. Returns the conversations to
   * catch up on. On the first call after construction or `reset()` the
   * result is empty (initial login has no missed history); on subsequent
   * calls it contains every tracked DM peer and room.
   */
  onSessionStarted(): CatchupEntry[] {
    if (!this.sessionStartedOnce) {
      this.sessionStartedOnce = true;
      return [];
    }
    const entries: CatchupEntry[] = [];
    for (const key of this.dmLastSeen.keys()) entries.push({ kind: "dm", key });
    for (const key of this.roomLastSeen.keys()) entries.push({ kind: "room", key });
    return entries;
  }

  /** Clear all state (cursors and the session-started flag). */
  reset(): void {
    this.dmLastSeen.clear();
    this.roomLastSeen.clear();
    this.sessionStartedOnce = false;
  }
}

function advance(map: Map<string, string>, key: string, timestamp: string): void {
  const current = map.get(key);
  if (current === undefined || timestamp > current) {
    map.set(key, timestamp);
  }
}
