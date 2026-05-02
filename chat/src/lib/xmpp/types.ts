import type { WaddleChannelType } from "@/lib/channel-types";
import type { ExtensionAnnotation } from "@/lib/chat-ui";
import type { WaddleEncryptedFile } from "./extensions/encrypted-file";

/** Shared types for the XMPP client layer. */

export interface XmppStatusSnapshot {
  state: "online" | "offline" | "reconnecting" | "error";
  detail: string;
}

/**
 * XEP-0198 Stream Management session lifecycle:
 * - "resumed": the prior stream was resumed server-side, no gap.
 * - "fresh": a new session was bound (either first connect, or resume failed).
 *   Consumers may re-fetch MAM to close any gap.
 */
export type SessionLifecycleEvent = { type: "resumed" } | { type: "fresh" };

export type MamPageParam =
  | { type: "latest" }
  | { type: "before"; before: string };

export interface MamHistoryPage<T> {
  messages: T[];
  firstArchiveId?: string;
  lastArchiveId?: string;
  complete: boolean;
}

/**
 * Transport- or protocol-level failure classification, surfaced via
 * `BrowserXmppClient.onError` for telemetry / diagnostics. Different
 * from `XmppStatusSnapshot` which is the user-facing UI state — an
 * error can fire without flipping UI state (e.g. a recoverable stream
 * error while already reconnecting), and the UI can go to
 * `reconnecting` without any discrete error (pure network drop).
 */
export type XmppErrorKind =
  | "stream"
  | "auth"
  | "connect-timeout"
  | "member-query";

export interface XmppErrorEvent {
  kind: XmppErrorKind;
  /** Whether the client expects to recover on its own without UI intervention. */
  recoverable: boolean;
  /** Short, human-readable reason. Safe to log in cleartext. */
  detail: string;
  /** Original error object if one was caught. */
  cause?: unknown;
  /** XMPP stream-error condition when `kind === "stream"`. */
  condition?: string;
}

export interface ListRoomMembersOptions {
  /**
   * Canonical room JID discovered from disco/topology. Prefer this over
   * reconstructing `${channelId}@muc.domain` when callers have it.
   */
  roomJid?: string;
}

export interface ReplyPreview {
  /** Stanza id of the parent message being replied to. */
  id: string;
  /** Author JID/occupant of the parent message, if known. */
  author?: string;
  /** Optional rendered preview text (populated by the UI, not the wire). */
  preview?: string;
}

export interface LiveRoomMessage {
  id: string;
  wireIds?: string[];
  /** XEP-0308 correction target: original sender message id/origin-id. */
  correctionTargetId?: string;
  roomJid: string;
  nick: string;
  body: string;
  createdAt: string;
  type: "message" | "subject";
  /** XEP-0308 */
  replacesId?: string;
  /** XEP-0424 */
  retractsId?: string;
  /** XEP-0372 */
  mentions?: string[];
  /** XEP-0394 */
  markup?: import("@/lib/chat-ui").MarkupSpan[];
  /** XEP-0372 */
  references?: import("@/lib/chat-ui").MessageReference[];
  /** XEP-0446/0447 — zero or more attachments. */
  sharedFiles?: SharedFileInfo[];
  /** Waddle unified extension framework annotations. */
  extensionAnnotations?: ExtensionAnnotation[];
  /** XEP-0449 */
  isSticker?: boolean;
  /** XEP-0513 */
  broadcastMention?: "everyone" | "here";
  /** XEP-0461 */
  replyTo?: ReplyPreview;
  /** RFC 6121 / XEP-0201 */
  threadId?: string;
  parentThreadId?: string;
  /** Waddle thread metadata */
  forumPostKind?: "topic" | "reply";
  forumTitle?: string;
  forumThreadTitle?: string;
  /** XEP-0313 MUC archive real JID from muc#user item@jid, when present. */
  authorRealJid?: string;
  /** XEP-0444 groupchat reaction target: room-assigned XEP-0359 stanza-id. */
  reactionTargetId?: string;
  /**
   * XEP-0461 §3.2 replyable id. For groupchat: only set when the message
   * carries a room-assigned XEP-0359 stanza-id. Absent means the message
   * cannot be replied to (the spec is explicit about this). For DMs:
   * origin-id then message @id, per the same section.
   */
  replyableId?: string;
  _reactionTarget?: string;
  _reactionEmojis?: string[];
  _reactionSenderId?: string;
}

/** A direct message received/sent via type:"chat" stanzas */
export interface LiveDmMessage {
  id: string;
  wireIds?: string[];
  /** XEP-0308 correction target: original sender message id/origin-id. */
  correctionTargetId?: string;
  peerJid: string;
  fromJid: string;
  nick: string;
  body: string;
  createdAt: string;
  type: "message";
  replacesId?: string;
  retractsId?: string;
  mentions?: string[];
  /** XEP-0394 */
  markup?: import("@/lib/chat-ui").MarkupSpan[];
  /** XEP-0372 */
  references?: import("@/lib/chat-ui").MessageReference[];
  sharedFiles?: SharedFileInfo[];
  extensionAnnotations?: ExtensionAnnotation[];
  isSticker?: boolean;
  /** XEP-0461 */
  replyTo?: ReplyPreview;
  /** RFC 6121 / XEP-0201 */
  threadId?: string;
  parentThreadId?: string;
  /** Waddle thread metadata */
  forumPostKind?: "topic" | "reply";
  forumTitle?: string;
  forumThreadTitle?: string;
  /** XEP-0461 §3.2 replyable id (origin-id then message @id for DMs). */
  replyableId?: string;
  _reactionTarget?: string;
  _reactionEmojis?: string[];
}

export interface SharedFileInfo {
  name?: string;
  mediaType?: string;
  size?: number;
  width?: number;
  height?: number;
  desc?: string;
  url: string;
  disposition: "inline" | "attachment";
  encrypted?: WaddleEncryptedFile;
}

export type ChatStateType = "active" | "composing" | "paused" | "inactive" | "gone";

export interface OccupantHat {
  title: string;
  uri: string;
}

export type RoomHats = Record<string, OccupantHat[]>;

export type OccupantPresence = "online" | "away" | "dnd" | "offline";
export type RoomPresence = Record<string, OccupantPresence>;

export interface DisplayedEvent {
  roomJid: string;
  nick: string;
  messageId: string;
}

export interface ReactionEvent {
  roomJid: string;
  nick: string;
  authorRealJid?: string;
  messageId: string;
  emojis: string[];
}

export interface DmConversation {
  peerJid: string;
  peerUsername: string;
  peerAvatarUrl?: string | null;
  lastMessageBody?: string;
  lastMessageAt?: string;
  unreadCount: number;
  presenceShow?: "available" | "away" | "xa" | "dnd" | "offline";
}

export interface ChatStateEvent {
  roomJid: string;
  nick: string;
  state: ChatStateType;
}

export interface DmChatStateEvent {
  peerJid: string;
  state: ChatStateType;
}

export interface DmDisplayedEvent {
  peerJid: string;
  messageId: string;
}

export interface DmReactionEvent {
  peerJid: string;
  messageId: string;
  emojis: string[];
}

export interface PresenceUpdateEvent {
  bareJid: string;
  show: "available" | "away" | "xa" | "dnd" | "offline";
  status?: string;
}

export interface RosterContact {
  jid: string;
  name?: string;
  username: string;
  subscription: "none" | "to" | "from" | "both" | "remove";
  groups: string[];
  presenceShow?: PresenceUpdateEvent["show"];
}

export interface DiscoveredChannel {
  id: string;
  name: string;
  jid?: string;
  channelType: WaddleChannelType;
  position: number;
  spaceId?: string;
  standalone?: boolean;
  features?: string[];
}

export interface DiscoveredSpace {
  id: string;
  name: string;
  role?: "owner" | "admin" | "moderator" | "member" | null;
}

export interface DiscoveredTopology {
  spaces: DiscoveredSpace[];
  rooms: DiscoveredChannel[];
  serverRole?: "owner" | "admin" | "moderator" | "member" | null;
  services?: {
    muc?: string;
    spaces?: string;
  };
}

/** Cross-room activity event with optional mention data for notifications. */
export interface RoomActivityEvent {
  roomJid: string;
  nick: string;
  body: string;
  /** XEP-0372 */
  mentions?: string[];
  /** XEP-0513 */
  broadcastMention?: "everyone" | "here";
}
