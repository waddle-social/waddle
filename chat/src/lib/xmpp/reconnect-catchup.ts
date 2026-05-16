import { compareTimelineTimestamps } from "@/lib/timeline-timestamps";

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
 *      tracked conversations via XEP-0059 `after` when an archive UID is
 *      known, or a timestamp fallback for conversations seen only live).
 *
 * DM peers and rooms are tracked in separate namespaces so the two can never
 * collide even if a bare JID were somehow valid in both worlds.
 *
 * Kept as a tiny pure class so the orchestration in `BrowserXmppClient` has
 * zero state of its own and the logic is trivially testable.
 */
type CatchupEntry =
  | { kind: "dm"; key: string; after?: string; since?: string; seenIds?: string[] }
  | { kind: "room"; key: string; after?: string; since?: string; seenIds?: string[] };

type SeenCursor = {
  timestamp: string;
  archiveId?: string;
  seenIds?: string[];
};

export class ReconnectCatchup {
  private readonly dmLastSeen = new Map<string, SeenCursor>();
  private readonly roomLastSeen = new Map<string, SeenCursor>();
  private sessionStartedOnce = false;

  /**
   * Record that a DM with `peerBareJid` was seen at `timestamp`. Only
   * advances the cursor — out-of-order or older arrivals are ignored so the
   * next catch-up can't re-pull already-applied messages.
   */
  recordDmSeen(peerBareJid: string, timestamp: string, archiveId?: string, seenIds?: ReadonlyArray<string>): void {
    advance(this.dmLastSeen, peerBareJid, timestamp, archiveId, seenIds);
  }

  /** As `recordDmSeen`, but for a MUC room JID. */
  recordRoomSeen(roomBareJid: string, timestamp: string, archiveId?: string, seenIds?: ReadonlyArray<string>): void {
    advance(this.roomLastSeen, roomBareJid, timestamp, archiveId, seenIds);
  }

  getDmLastSeen(peerBareJid: string): string | undefined {
    return this.dmLastSeen.get(peerBareJid)?.timestamp;
  }

  getRoomLastSeen(roomBareJid: string): string | undefined {
    return this.roomLastSeen.get(roomBareJid)?.timestamp;
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
    for (const [key, cursor] of this.dmLastSeen) {
      entries.push(catchupEntry("dm", key, cursor));
    }
    for (const [key, cursor] of this.roomLastSeen) {
      entries.push(catchupEntry("room", key, cursor));
    }
    return entries;
  }

  /** Clear all state (cursors and the session-started flag). */
  reset(): void {
    this.dmLastSeen.clear();
    this.roomLastSeen.clear();
    this.sessionStartedOnce = false;
  }
}

function catchupEntry(kind: "dm", key: string, cursor: SeenCursor): CatchupEntry;
function catchupEntry(kind: "room", key: string, cursor: SeenCursor): CatchupEntry;
function catchupEntry(kind: "dm" | "room", key: string, cursor: SeenCursor): CatchupEntry {
  if (!cursor.archiveId) {
    return {
      kind,
      key,
      since: cursor.timestamp,
      ...(cursor.seenIds?.length ? { seenIds: cursor.seenIds } : {}),
    };
  }
  return {
    kind,
    key,
    after: cursor.archiveId,
    ...(cursor.seenIds?.length ? { seenIds: cursor.seenIds } : {}),
  };
}

function advance(
  map: Map<string, SeenCursor>,
  key: string,
  timestamp: string,
  archiveId?: string,
  seenIds?: ReadonlyArray<string>,
): void {
  const normalizedTimestamp = normalizeTimestamp(timestamp);
  const current = map.get(key);
  const ordering = current ? compareTimestamps(normalizedTimestamp, current.timestamp) : 1;
  if (current === undefined || ordering > 0) {
    const nextArchiveId = archiveId ?? current?.archiveId;
    map.set(key, {
      timestamp: normalizedTimestamp,
      ...(nextArchiveId ? { archiveId: nextArchiveId } : {}),
      ...nonEmptySeenIds(seenIds),
    });
    return;
  }
  if (ordering === 0) {
    const nextSeenIds = mergeSeenIds(current.seenIds, seenIds);
    map.set(key, {
      ...current,
      ...(archiveId ? { archiveId } : {}),
      ...nonEmptySeenIds(nextSeenIds),
    });
  }
}

function nonEmptySeenIds(seenIds: ReadonlyArray<string> | undefined): Partial<Pick<SeenCursor, "seenIds">> {
  const normalized = mergeSeenIds(undefined, seenIds);
  return normalized.length > 0 ? { seenIds: normalized } : {};
}

function mergeSeenIds(
  current: ReadonlyArray<string> | undefined,
  next: ReadonlyArray<string> | undefined,
): string[] {
  return Array.from(new Set([...(current ?? []), ...(next ?? [])].filter(Boolean)));
}

function normalizeTimestamp(timestamp: string): string {
  const timestampMs = Date.parse(timestamp);
  return Number.isFinite(timestampMs) ? new Date(timestampMs).toISOString() : timestamp;
}

function compareTimestamps(left: string, right: string): number {
  return compareTimelineTimestamps(left, right);
}
