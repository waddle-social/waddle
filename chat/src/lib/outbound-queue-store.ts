import type { MarkupSpan, MessageReference } from "@/lib/chat-ui";
import type { OutboundFileAttachment, ReplyTarget } from "@/lib/xmpp/send-types";
import { reportError } from "@/lib/telemetry";
import { bareJidKey, fullJidIdentityKey } from "@/lib/xmpp/jid";
import type { DmConversationScope } from "@/lib/xmpp/reconnect-catchup";

const PREFIX = "waddle.chat.outbound-queue";

/**
 * Drop persisted queued messages older than this window. A user
 * who types a message, closes the tab before it sends, and then
 * comes back days later does not want yesterday's draft replayed
 * with its stale `createdAt` — it would insert ahead of every
 * message received since, mis-positioning the row in the timeline
 * and surprising the user with a send they don't remember
 * intending. 7 days bounds the per-account storage footprint
 * without losing same-session retries.
 */
const QUEUE_TTL_MS = 7 * 24 * 60 * 60 * 1000;

interface PersistedQueuedMessageBase {
  id: string;
  createdAt: string;
  body: string;
  markup?: MarkupSpan[];
  references?: MessageReference[];
  mentionJidsByNick?: Record<string, string>;
  files?: OutboundFileAttachment[];
  replyTo?: ReplyTarget;
  threadId?: string;
  parentThreadId?: string;
}

export interface PersistedQueuedRoomMessage extends PersistedQueuedMessageBase {
  kind: "room";
  roomJid: string;
  threadCreate?: { title: string };
  threadReply?: { threadId: string };
}

export interface PersistedQueuedDmMessage extends PersistedQueuedMessageBase {
  kind: "dm";
  /**
   * XEP-0045 §7.5 (#1256): for a MUC private message this is the FULL
   * occupant JID (`room@service/nick`) and `mucPm` is set — the drain
   * must send to it verbatim; bare-folding would broadcast to the room.
   */
  peerJid: string;
  mucPm?: boolean;
}

type PersistedQueuedMessage =
  | PersistedQueuedRoomMessage
  | PersistedQueuedDmMessage;

function storage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function queueKey(accountKey: string): string {
  return `${PREFIX}.${accountKey}`;
}

function sortQueue(messages: PersistedQueuedMessage[]): PersistedQueuedMessage[] {
  return [...messages].sort((a, b) => {
    const createdAtOrder = a.createdAt.localeCompare(b.createdAt);
    return createdAtOrder !== 0 ? createdAtOrder : a.id.localeCompare(b.id);
  });
}

function isPersistedQueuedMessage(value: unknown): value is PersistedQueuedMessage {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  if (
    typeof candidate.id !== "string"
    || typeof candidate.createdAt !== "string"
    || typeof candidate.body !== "string"
    || typeof candidate.kind !== "string"
  ) {
    return false;
  }

  if (candidate.kind === "room") {
    return typeof candidate.roomJid === "string";
  }
  if (candidate.kind === "dm") {
    return typeof candidate.peerJid === "string";
  }

  return false;
}

function readQueue(accountKey: string): PersistedQueuedMessage[] {
  const s = storage();
  if (!s) return [];
  try {
    const raw = s.getItem(queueKey(accountKey));
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const persisted = parsed.filter(isPersistedQueuedMessage);
    const strippedLegacyPreviewTokens = persisted.some(
      (entry) =>
        "linkPreviewToken" in (entry as unknown as Record<string, unknown>)
        || "linkPreviewExpiresAt" in (entry as unknown as Record<string, unknown>),
    );
    const all = sortQueue(persisted.map(stripLegacyPreviewToken));
    if (strippedLegacyPreviewTokens) writeQueue(accountKey, all);
    return pruneStaleEntries(s, accountKey, all);
  } catch (err) {
    // Storage read failure usually means corrupt JSON or privacy-mode
    // localStorage. Surface to Faro but keep the app working — we just
    // treat it as an empty queue.
    void err;
    reportError({ kind: "storage.read", reason: "read-failed", area: "outbound-queue" });
    return [];
  }
}

function stripLegacyPreviewToken(message: PersistedQueuedMessage): PersistedQueuedMessage {
  const { linkPreviewToken: _token, linkPreviewExpiresAt: _expiresAt, ...rest } =
    message as PersistedQueuedMessage & {
      linkPreviewToken?: unknown;
      linkPreviewExpiresAt?: unknown;
    };
  return rest;
}

/**
 * Filter out queue entries past the TTL and rewrite the snapshot if
 * any were dropped. This keeps the persisted footprint bounded over
 * the lifetime of the account without needing a separate cleanup
 * pass — the next read naturally trims stale rows.
 *
 * Pruning is silent to the UI: the row never enters `messages.value`
 * (the prune fires inside the read that builds the timeline), so
 * there is no "stuck queued" row left dangling. The trade-off is
 * that a user who reloads a long-suspended tab will not see a
 * failure notification for the drop — by design, because 7-day-old
 * drafts are almost always abandoned. Pruning IS logged via
 * `reportError` so a sudden spike in pruned-counts surfaces in
 * telemetry.
 */
function pruneStaleEntries(
  s: Storage,
  accountKey: string,
  all: PersistedQueuedMessage[],
): PersistedQueuedMessage[] {
  const cutoff = Date.now() - QUEUE_TTL_MS;
  const fresh = all.filter((entry) => parseCreatedAt(entry.createdAt) >= cutoff);
  if (fresh.length === all.length) return all;
  const dropped = all.length - fresh.length;
  try {
    if (fresh.length === 0) {
      s.removeItem(queueKey(accountKey));
    } else {
      s.setItem(queueKey(accountKey), JSON.stringify(fresh));
    }
  } catch (err) {
    // Best-effort prune; surface to Faro but still hand back the
    // pruned list to the caller so the stale entries are not used
    // in this session even if persistence couldn't be updated.
    void err;
    reportError({
      kind: "storage.write",
      reason: "write-failed",
      area: "outbound-queue",
      dropped,
    });
    return fresh;
  }
  // Successful prune — surface to telemetry so a spike in pruned
  // counts is visible to operators. `storage.write` is the closest
  // existing `ErrorKind` (we wrote the pruned snapshot back), and
  // `recoverable: true` keeps this in the informational tier rather
  // than alerting on it.
  reportError({
    kind: "storage.write",
    reason: "stale-entries-pruned",
    area: "outbound-queue",
    dropped,
  });
  return fresh;
}

function parseCreatedAt(value: string): number {
  const ms = Date.parse(value);
  // If the timestamp is unparseable, treat the entry as fresh —
  // refusing to prune is safer than dropping a legitimate send.
  return Number.isFinite(ms) ? ms : Date.now();
}

function writeQueue(accountKey: string, messages: PersistedQueuedMessage[]): void {
  const s = storage();
  if (!s) return;
  try {
    const sorted = sortQueue(messages);
    if (sorted.length === 0) {
      s.removeItem(queueKey(accountKey));
      return;
    }
    s.setItem(queueKey(accountKey), JSON.stringify(sorted));
  } catch (err) {
    // Best effort only — if storage is unavailable the in-memory optimistic
    // state still reflects the queued send for the current page lifetime.
    // Still worth reporting: localStorage quota errors are a leading cause
    // of silent message-loss across reloads.
    const name = err instanceof Error ? err.name : "";
    void err;
    reportError({
      kind: name === "QuotaExceededError" ? "storage.quota" : "storage.write",
      reason: name === "QuotaExceededError" ? "quota-exceeded" : "write-failed",
      area: "outbound-queue",
      queueSize: messages.length,
    });
  }
}

export function listQueuedMessages(accountKey: string): PersistedQueuedMessage[] {
  return readQueue(accountKey);
}

export function listQueuedRoomMessages(
  accountKey: string,
  roomJid: string,
): PersistedQueuedRoomMessage[] {
  return readQueue(accountKey).filter(
    (message): message is PersistedQueuedRoomMessage =>
      message.kind === "room" && message.roomJid === roomJid,
  );
}

export function listQueuedDmMessages(
  accountKey: string,
  peerJid: string,
  scope: DmConversationScope,
): PersistedQueuedDmMessage[] {
  const requestedMucPm = scope === "muc-occupant";
  const requestedKey = dmQueuePeerKey(peerJid, requestedMucPm);
  return readQueue(accountKey).filter(
    (message): message is PersistedQueuedDmMessage => {
      if (message.kind !== "dm") return false;
      const rowMucPm = message.mucPm === true;
      if (rowMucPm !== requestedMucPm) return false;
      // Before the scope bit existed, account DMs were already persisted
      // bare. A resource-bearing row without `mucPm` is therefore ambiguous
      // legacy data and must fail closed instead of aliasing a room occupant.
      if (!rowMucPm && message.peerJid.includes("/")) return false;
      return dmQueuePeerKey(message.peerJid, rowMucPm) === requestedKey;
    },
  );
}

function dmQueuePeerKey(peerJid: string, mucPm: boolean): string {
  return mucPm ? fullJidIdentityKey(peerJid) : bareJidKey(peerJid);
}

export function enqueueQueuedMessage(
  accountKey: string,
  message: PersistedQueuedMessage,
): void {
  const next = readQueue(accountKey).filter((entry) => entry.id !== message.id);
  next.push(message.kind === "dm"
    ? {
        ...message,
        peerJid: dmQueuePeerKey(message.peerJid, message.mucPm === true),
      }
    : message);
  writeQueue(accountKey, next);
}

export function removeQueuedMessage(accountKey: string, messageId: string): void {
  const next = readQueue(accountKey).filter((message) => message.id !== messageId);
  writeQueue(accountKey, next);
}

export function countQueuedMessages(accountKey: string): number {
  return readQueue(accountKey).length;
}
