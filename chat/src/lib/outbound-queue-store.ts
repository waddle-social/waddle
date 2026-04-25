import type { MarkupSpan, MessageReference } from "@/lib/chat-ui";
import type { OutboundFileAttachment, ReplyTarget } from "@/lib/xmpp/messaging";
import { reportError } from "@/lib/telemetry";

const PREFIX = "waddle.chat.outbound-queue";

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
  peerJid: string;
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
    return sortQueue(parsed.filter(isPersistedQueuedMessage));
  } catch (err) {
    // Storage read failure usually means corrupt JSON or privacy-mode
    // localStorage. Surface to Faro but keep the app working — we just
    // treat it as an empty queue.
    reportError("storage.read", err, {
      recoverable: true,
      detail: "outbound-queue read failed",
      accountKey,
    });
    return [];
  }
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
    const kind = name === "QuotaExceededError" ? "storage.quota" : "storage.write";
    reportError(kind, err, {
      recoverable: true,
      detail: "outbound-queue write failed",
      accountKey,
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
): PersistedQueuedDmMessage[] {
  return readQueue(accountKey).filter(
    (message): message is PersistedQueuedDmMessage =>
      message.kind === "dm" && message.peerJid === peerJid,
  );
}

export function enqueueQueuedMessage(
  accountKey: string,
  message: PersistedQueuedMessage,
): void {
  const next = readQueue(accountKey).filter((entry) => entry.id !== message.id);
  next.push(message);
  writeQueue(accountKey, next);
}

export function removeQueuedMessage(accountKey: string, messageId: string): void {
  const next = readQueue(accountKey).filter((message) => message.id !== messageId);
  writeQueue(accountKey, next);
}

export function countQueuedMessages(accountKey: string): number {
  return readQueue(accountKey).length;
}
