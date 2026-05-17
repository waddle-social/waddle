import type { WaddleChannelType } from "@/lib/channel-types";
import type { PinPermission } from "@/lib/chat-types";
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
  | { type: "before"; before: string }
  | { type: "after"; after: string };

export type MamThreadPageParam =
  | { type: "latest" }
  | { type: "before"; before: string };

export interface MamHistoryPage<T> {
  messages: T[];
  firstArchiveId?: string;
  lastArchiveId?: string;
  complete: boolean;
}

export interface MessageSearchResult {
  id: string;
  archiveId?: string;
  nick: string;
  body: string;
  createdAt: string;
  threadId?: string;
  parentThreadId?: string;
  roomJid?: string;
  peerJid?: string;
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
  | "history"
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
  /** XEP-0313 archive UID from the MAM result envelope, when known. */
  archiveId?: string;
  wireIds?: string[];
  /** XEP-0308 correction target: original sender message id/origin-id. */
  correctionTargetId?: string;
  fromJid?: string;
  roomJid: string;
  nick: string;
  body: string;
  createdAt: string;
  type: "message" | "subject" | "pin-event";
  /** #414: pin/unpin action carried by a `<pin-event/>` system
   * message. Set when `type === "pin-event"`. */
  pinEventAction?: "pinned" | "unpinned";
  /** XEP-0308 */
  replacesId?: string;
  /** XEP-0424 */
  retractsId?: string;
  retractionId?: string;
  isRetracted?: boolean;
  /** XEP-0425 moderation broadcast target. */
  moderationTargetId?: string;
  moderatedBy?: string;
  moderationReason?: string;
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
  /** XEP-0428: message body is fallback text for the Waddle extension payload. */
  extensionBodyFallback?: boolean;
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
  /** XEP-0359 room-assigned stanza-id, used by XEP-0490 MDS for MUC. */
  stanzaId?: string;
  /** Bare JID that injected `stanzaId` (the room for MUC). */
  stanzaIdBy?: string;
  _reactionTarget?: string;
  _reactionEmojis?: string[];
  _reactionSenderId?: string;
}

/** A direct message received/sent via type:"chat" stanzas */
export interface LiveDmMessage {
  id: string;
  /** XEP-0313 archive UID from the MAM result envelope, when known. */
  archiveId?: string;
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
  retractionId?: string;
  isRetracted?: boolean;
  mentions?: string[];
  /** XEP-0394 */
  markup?: import("@/lib/chat-ui").MarkupSpan[];
  /** XEP-0372 */
  references?: import("@/lib/chat-ui").MessageReference[];
  sharedFiles?: SharedFileInfo[];
  extensionAnnotations?: ExtensionAnnotation[];
  extensionBodyFallback?: boolean;
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
  /** XEP-0359 user-server-injected stanza-id, used by XEP-0490 MDS. */
  stanzaId?: string;
  /** Bare JID that injected `stanzaId` (the user's own server for DMs). */
  stanzaIdBy?: string;
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

/** XEP-0045 §5.2 — persistent room-relationship category. Authority,
 * not descriptive metadata. Carried by `<x xmlns='muc#user'><item
 * affiliation='…'/>` on every MUC presence. */
export type MucAffiliation = "owner" | "admin" | "member" | "outcast" | "none";

/** XEP-0045 §5.1 — runtime session-scoped permission category.
 * Authority, not descriptive metadata. Carried by `<x xmlns='muc#user'>
 * <item role='…'/>` on every MUC presence. */
export type MucRole = "moderator" | "participant" | "visitor" | "none";

/** Pairs the two MUC authority layers per occupant. Distinct from
 * `OccupantHat` (XEP-0317), which is descriptive social metadata
 * with no protocol semantics. */
export interface OccupantAuthority {
  affiliation: MucAffiliation;
  role: MucRole;
}

/** Per-room map of occupant nick → their current MUC authority,
 * updated from each inbound MUC presence stanza. */
export type RoomAuthority = Record<string, OccupantAuthority>;

/** Narrow an untyped string (e.g. from the WASM bridge) to the
 * MucAffiliation union, falling back to "none" for unrecognised
 * input. */
export function parseMucAffiliation(raw: string | undefined | null): MucAffiliation {
  switch (raw) {
    case "owner":
    case "admin":
    case "member":
    case "outcast":
      return raw;
    default:
      return "none";
  }
}

/** Narrow an untyped string (e.g. from the WASM bridge) to the
 * MucRole union, falling back to "none" for unrecognised input. */
export function parseMucRole(raw: string | undefined | null): MucRole {
  switch (raw) {
    case "moderator":
    case "participant":
    case "visitor":
      return raw;
    default:
      return "none";
  }
}

export type OccupantPresence = "online" | "away" | "dnd" | "offline";
export type RoomPresence = Record<string, OccupantPresence>;

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
  /** Conversation partner's bare JID — i.e. the key the local UI uses
   * to address the conversation. For carbon-forwarded self-sent
   * reactions this is the recipient (`to`), not the sender (`from`). */
  peerJid: string;
  /** Bare JID of the user who emitted the reaction stanza. Distinct
   * from `peerJid` for self-sent carbon-forwarded reactions, where
   * `peerJid` is normalized to the conversation partner. */
  reactorJid: string;
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
  /** #422: room's current pin permission, hydrated from the
   * `urn:waddle:room-metadata` extended-disco form so non-owners
   * can render the Pin action correctly under the `anyone`
   * policy without needing owner-config access. */
  pinPermission?: PinPermission;
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
