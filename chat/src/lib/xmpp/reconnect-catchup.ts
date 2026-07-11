import { compareTimelineTimestamps } from "@/lib/timeline-timestamps";
import { bareJidKey, fullJidIdentityKey } from "@/lib/xmpp/jid";

import {
  nullResumePersistence,
  type PersistedReconnectCatchup,
  type ResumePersistence,
} from "./resume-persistence";

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
 * State is hydrated from / persisted to a `ResumePersistence` adapter so
 * cursors survive a full page reload — without persistence, a hard refresh
 * loses every cursor and the next `onSessionStarted()` returns `[]`, leaving
 * any messages received while the tab was gone unrecovered until the user
 * manually scrolls back (PR3 in the tab-restore plan).
 */
export type ReconnectCatchupEntry =
  | { kind: "dm"; key: string; scope: DmConversationScope; after?: string; since?: string; seenIds?: string[] }
  | { kind: "room"; key: string; after?: string; since?: string; seenIds?: string[] };

type SeenCursor = {
  timestamp: string;
  scope?: DmConversationScope;
  archiveId?: string;
  archiveTimestamp?: string;
  archiveSeenIds?: string[];
  seenIds?: string[];
};

export type DmConversationScope = "account" | "muc-occupant";

function dmCursorKey(peerJid: string, scope: DmConversationScope): string {
  return scope === "muc-occupant"
    ? fullJidIdentityKey(peerJid)
    : bareJidKey(peerJid);
}

export class ReconnectCatchup {
  private readonly dmLastSeen = new Map<string, SeenCursor>();
  private readonly roomLastSeen = new Map<string, SeenCursor>();
  private sessionStartedOnce = false;
  private readonly persistence: ResumePersistence;
  /** Microtask-coalesced write to amortize the cost of localStorage writes
   * across a burst of arrivals. Bursts are common during MAM catch-up. */
  private persistScheduled = false;

  constructor(persistence: ResumePersistence = nullResumePersistence) {
    this.persistence = persistence;
    this.hydrate();
  }

  private hydrate(): void {
    const snapshot = this.persistence.loadCatchup();
    if (!snapshot) return;
    for (const [key, cursor] of snapshot.dmLastSeen) {
      const scope = cursor.scope ?? "account";
      this.dmLastSeen.set(dmCursorKey(key, scope), { ...cursor, scope });
    }
    for (const [key, cursor] of snapshot.roomLastSeen) this.roomLastSeen.set(key, { ...cursor });
    // A hydrated instance is by definition *not* a first-ever login —
    // it represents a prior tab session that left cursors behind. The
    // next `onSessionStarted()` must return those cursors so MAM
    // catch-up runs immediately after the page reload (otherwise the
    // persistence is dead weight: cursors are stored on every advance
    // but never consumed, which was the bug PR3 was supposed to fix).
    if (this.dmLastSeen.size > 0 || this.roomLastSeen.size > 0) {
      this.sessionStartedOnce = true;
    }
  }

  private schedulePersist(): void {
    if (this.persistScheduled || this.persistence === nullResumePersistence) return;
    this.persistScheduled = true;
    queueMicrotask(() => {
      this.persistScheduled = false;
      // Read-merge-write so two tabs of the same account don't
      // clobber each other's cursor advances. Pre-fix each tab held
      // its own in-memory map and serialized the whole thing on
      // every write — the last writer won, and the other tab's
      // progress was silently lost. Now we merge any remote state
      // that landed since our last write back into our in-memory
      // maps before persisting, so both tabs converge to the union
      // (per-key: higher timestamp wins, equal timestamps merge
      // `seenIds` — exactly the rule `advance()` already uses).
      const remote = this.persistence.loadCatchup();
      if (remote) this.absorbRemoteSnapshot(remote);
      const snapshot: PersistedReconnectCatchup = {
        dmLastSeen: Array.from(this.dmLastSeen.entries()),
        roomLastSeen: Array.from(this.roomLastSeen.entries()),
      };
      this.persistence.saveCatchup(snapshot);
    });
  }

  private absorbRemoteSnapshot(remote: PersistedReconnectCatchup): void {
    for (const [key, cursor] of remote.dmLastSeen) {
      const scope = cursor.scope ?? "account";
      advance(this.dmLastSeen, dmCursorKey(key, scope), cursor.timestamp, cursor.archiveId, cursor.seenIds, cursor.archiveTimestamp, cursor.archiveSeenIds, scope);
    }
    for (const [key, cursor] of remote.roomLastSeen) {
      advance(this.roomLastSeen, key, cursor.timestamp, cursor.archiveId, cursor.seenIds, cursor.archiveTimestamp, cursor.archiveSeenIds);
    }
  }

  /**
   * Record that a DM with `peerBareJid` was seen at `timestamp`. Only
   * advances the cursor — out-of-order or older arrivals are ignored so the
   * next catch-up can't re-pull already-applied messages.
   */
  recordDmSeen(peerBareJid: string, timestamp: string, archiveId?: string, seenIds?: ReadonlyArray<string>, scope: DmConversationScope = "account"): void {
    advance(this.dmLastSeen, dmCursorKey(peerBareJid, scope), timestamp, archiveId, seenIds, undefined, undefined, scope);
    this.schedulePersist();
  }

  /** As `recordDmSeen`, but for a MUC room JID. */
  recordRoomSeen(roomBareJid: string, timestamp: string, archiveId?: string, seenIds?: ReadonlyArray<string>): void {
    advance(this.roomLastSeen, roomBareJid, timestamp, archiveId, seenIds);
    this.schedulePersist();
  }

  getDmLastSeen(peerBareJid: string): string | undefined {
    const occupantCursor = this.dmLastSeen.get(dmCursorKey(peerBareJid, "muc-occupant"));
    if (occupantCursor?.scope === "muc-occupant") return occupantCursor.timestamp;
    return this.dmLastSeen.get(dmCursorKey(peerBareJid, "account"))?.timestamp;
  }

  getDmScope(peerJid: string): DmConversationScope | undefined {
    const occupantCursor = this.dmLastSeen.get(dmCursorKey(peerJid, "muc-occupant"));
    if (occupantCursor?.scope === "muc-occupant") return occupantCursor.scope;
    return this.dmLastSeen.get(dmCursorKey(peerJid, "account"))?.scope;
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
  onSessionStarted(): ReconnectCatchupEntry[] {
    if (!this.sessionStartedOnce) {
      this.sessionStartedOnce = true;
      return [];
    }
    const entries: ReconnectCatchupEntry[] = [];
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
    this.persistence.clearCatchup();
  }
}

function catchupEntry(kind: "dm", key: string, cursor: SeenCursor): ReconnectCatchupEntry;
function catchupEntry(kind: "room", key: string, cursor: SeenCursor): ReconnectCatchupEntry;
function catchupEntry(kind: "dm" | "room", key: string, cursor: SeenCursor): ReconnectCatchupEntry {
  if (!cursor.archiveId) {
    const rest = {
      since: cursor.timestamp,
      ...(cursor.seenIds?.length ? { seenIds: cursor.seenIds } : {}),
    };
    return kind === "dm"
      ? { kind, key, scope: cursor.scope ?? "account", ...rest }
      : { kind, key, ...rest };
  }
  const seenIds = mergeSeenIds(cursor.archiveSeenIds, cursor.seenIds);
  const rest = {
    after: cursor.archiveId,
    since: cursor.archiveTimestamp ?? cursor.timestamp,
    ...(seenIds.length ? { seenIds } : {}),
  };
  return kind === "dm"
    ? { kind, key, scope: cursor.scope ?? "account", ...rest }
    : { kind, key, ...rest };
}

function advance(
  map: Map<string, SeenCursor>,
  key: string,
  timestamp: string,
  archiveId?: string,
  seenIds?: ReadonlyArray<string>,
  archiveTimestamp?: string,
  archiveSeenIds?: ReadonlyArray<string>,
  scope?: DmConversationScope,
): void {
  const normalizedTimestamp = normalizeTimestamp(timestamp);
  const normalizedArchiveTimestamp = archiveTimestamp ? normalizeTimestamp(archiveTimestamp) : undefined;
  const current = map.get(key);
  const ordering = current ? compareTimestamps(normalizedTimestamp, current.timestamp) : 1;
  if (current === undefined || ordering > 0) {
    const nextArchiveId = archiveId ?? current?.archiveId;
    const nextArchiveTimestamp = archiveId ? (normalizedArchiveTimestamp ?? normalizedTimestamp) : current?.archiveTimestamp;
    const preservedArchiveSeenIds = current?.archiveSeenIds ?? (current?.archiveId ? current.seenIds : undefined);
    const nextArchiveSeenIds = archiveId
      ? mergeSeenIds(archiveSeenIds, seenIds)
      : mergeSeenIds(preservedArchiveSeenIds, archiveSeenIds);
    const nextSeenIds = nextArchiveId && !archiveId
      ? mergeSeenIds(current?.seenIds, seenIds)
      : mergeSeenIds(undefined, seenIds);
    map.set(key, {
      timestamp: normalizedTimestamp,
      ...(scope ? { scope } : current?.scope ? { scope: current.scope } : {}),
      ...(nextArchiveId ? { archiveId: nextArchiveId } : {}),
      ...(nextArchiveTimestamp ? { archiveTimestamp: nextArchiveTimestamp } : {}),
      ...nonEmptyArchiveSeenIds(nextArchiveSeenIds),
      ...nonEmptySeenIds(nextSeenIds),
    });
    return;
  }
  if (ordering === 0) {
    const nextSeenIds = mergeSeenIds(current.seenIds, seenIds);
    const nextArchiveSeenIds = archiveId
      ? mergeSeenIds(current.archiveSeenIds, mergeSeenIds(archiveSeenIds, seenIds))
      : mergeSeenIds(current.archiveSeenIds, archiveSeenIds);
    map.set(key, {
      ...current,
      ...(scope ? { scope } : {}),
      ...(archiveId ? { archiveId } : {}),
      ...(archiveId ? { archiveTimestamp: normalizedArchiveTimestamp ?? normalizedTimestamp } : {}),
      ...nonEmptyArchiveSeenIds(nextArchiveSeenIds),
      ...nonEmptySeenIds(nextSeenIds),
    });
  }
}

function nonEmptyArchiveSeenIds(seenIds: ReadonlyArray<string> | undefined): Partial<Pick<SeenCursor, "archiveSeenIds">> {
  const normalized = mergeSeenIds(undefined, seenIds);
  return normalized.length > 0 ? { archiveSeenIds: normalized } : {};
}

function nonEmptySeenIds(seenIds: ReadonlyArray<string> | undefined): Partial<Pick<SeenCursor, "seenIds">> {
  const normalized = mergeSeenIds(undefined, seenIds);
  return normalized.length > 0 ? { seenIds: normalized } : {};
}

// `seenIds` accumulates message ids at the boundary timestamp for
// exact-equal-second dedupe (see `advance` below). In busy MUCs many
// messages share the same wall-clock second, so without a cap this
// would grow without bound and eventually blow localStorage's
// per-origin quota. 64 is well above any realistic same-second
// burst; older ids fall off — they only matter for messages whose
// timestamp matches the cursor exactly, and once the cursor advances
// past them dedupe is no longer needed.
const SEEN_IDS_CAP = 64;

function mergeSeenIds(
  current: ReadonlyArray<string> | undefined,
  next: ReadonlyArray<string> | undefined,
): string[] {
  const merged = Array.from(new Set([...(current ?? []), ...(next ?? [])].filter(Boolean)));
  return merged.length > SEEN_IDS_CAP ? merged.slice(merged.length - SEEN_IDS_CAP) : merged;
}

function normalizeTimestamp(timestamp: string): string {
  const timestampMs = Date.parse(timestamp);
  return Number.isFinite(timestampMs) ? new Date(timestampMs).toISOString() : timestamp;
}

function compareTimestamps(left: string, right: string): number {
  return compareTimelineTimestamps(left, right);
}
