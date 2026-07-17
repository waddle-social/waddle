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
export const QUEUE_TTL_MS = 7 * 24 * 60 * 60 * 1000;

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

export type PersistedQueuedMessage =
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

function queueRowPrefix(accountKey: string): string {
  return `${PREFIX}.v2.${accountKey.length}:${accountKey}.`;
}

function queueRowKey(accountKey: string, messageId: string): string {
  return `${queueRowPrefix(accountKey)}${encodeURIComponent(messageId)}`;
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
    const prefix = queueRowPrefix(accountKey);
    const persisted: PersistedQueuedMessage[] = [];
    for (let index = 0; index < s.length; index += 1) {
      const key = s.key(index);
      if (!key?.startsWith(prefix)) continue;
      const raw = s.getItem(key);
      if (!raw) continue;
      const parsed: unknown = JSON.parse(raw);
      if (isPersistedQueuedMessage(parsed)) persisted.push(parsed);
    }

    const strippedLegacyPreviewTokens = persisted.some(
      (entry) =>
        "linkPreviewToken" in (entry as unknown as Record<string, unknown>)
        || "linkPreviewExpiresAt" in (entry as unknown as Record<string, unknown>),
    );
    const all = sortQueue(persisted.map(stripLegacyPreviewToken));
    if (strippedLegacyPreviewTokens) {
      for (const entry of all) writeProjection(s, accountKey, entry);
    }
    // IndexedDB owns retention and claim fencing. This synchronous storage is
    // only a paint projection, so it must never independently delete rows.
    return all;
  } catch (err) {
    // Storage read failure usually means corrupt JSON or privacy-mode
    // localStorage. Surface to Faro but keep the app working — we just
    // treat it as an empty queue.
    reportError("storage.read", err, {
      recoverable: true,
      detail: "outbound-queue read failed",
      storage_area: "outbound-queue",
    });
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

function writeProjection(
  s: Storage,
  accountKey: string,
  message: PersistedQueuedMessage,
): void {
  try {
    s.setItem(queueRowKey(accountKey, message.id), JSON.stringify(message));
  } catch (err) {
    // IndexedDB is the durable source of truth. This synchronous per-row
    // projection exists only so conversation composables can paint queued
    // rows without awaiting startup hydration.
    const name = err instanceof Error ? err.name : "";
    const kind = name === "QuotaExceededError" ? "storage.quota" : "storage.write";
    reportError(kind, err, {
      recoverable: true,
      detail: "outbound-queue projection write failed",
      storage_area: "outbound-queue-projection",
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
  const s = storage();
  if (!s) return;
  const normalized = message.kind === "dm"
    ? {
        ...message,
        peerJid: dmQueuePeerKey(message.peerJid, message.mucPm === true),
      }
    : message;
  writeProjection(s, accountKey, normalized);
}

export function removeQueuedMessage(accountKey: string, messageId: string): boolean {
  const s = storage();
  if (!s) return false;
  const key = queueRowKey(accountKey, messageId);
  const existed = s.getItem(key) !== null;
  if (!existed) return false;
  try {
    s.removeItem(key);
    return true;
  } catch (err) {
    reportError("storage.write", err, {
      recoverable: true,
      detail: "outbound-queue projection delete failed",
      storage_area: "outbound-queue-projection",
    });
    return false;
  }
}
