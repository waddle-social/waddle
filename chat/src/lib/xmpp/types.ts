import type { WaddleChannelType } from "@/lib/channel-types";
import type { PinPermission } from "@/lib/chat-types";
import type { CallThreadAnchor, ExtensionAnnotation } from "@/lib/chat-ui";
import type { TimestampSource } from "@/lib/timeline-timestamps";
import type { WaddleEncryptedFile } from "./extensions/encrypted-file";

/** Shared types for the XMPP client layer. */

export interface XmppStatusSnapshot {
  state: "online" | "offline" | "reconnecting" | "error";
  detail: string;
}

/**
 * Conversations the client's own reconnect catch-up will page via
 * XEP-0313 after this session-ready (bare JIDs, keyed the same way as
 * `ReconnectCatchup` cursors). Consumers whose active conversation is
 * covered must NOT fire their own MAM reload — two concurrent fetches
 * for the same conversation race on the timeline (#1180).
 */
type SessionCatchupCoverage = {
  dmJids: readonly string[];
  /** DM cursor keys authoritatively scoped as full MUC occupant JIDs. */
  dmOccupantJids?: readonly string[];
  roomJids: readonly string[];
};

/**
 * XEP-0198 Stream Management session lifecycle:
 * - "resumed": the prior stream was resumed server-side, no gap.
 * - "fresh": a new session was bound (either first connect, or resume failed).
 *   Consumers may re-fetch MAM to close any gap — unless `catchup`
 *   already covers the conversation.
 */
export type SessionLifecycleEvent =
  | { type: "resumed" }
  | { type: "fresh"; catchup: SessionCatchupCoverage };

export type MamPageParam =
  | { type: "latest"; start?: string }
  | { type: "before"; before: string; start?: string }
  | { type: "after"; after: string; start?: string };

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
  | "member-query"
  | "muc-join";

/** RFC 6120 §8.3.2 `type` attribute values of an `<error/>` element, plus
 * "unknown" for an unrecognised wire value (mirrors the wasm bridge's
 * `StanzaErrorType::as_str`). */
export type XmppStanzaErrorType =
  | "auth"
  | "cancel"
  | "continue"
  | "modify"
  | "wait"
  | "unknown";

export const XMPP_STREAM_ERROR_CONDITIONS = new Set([
  "bad-format",
  "bad-namespace-prefix",
  "conflict",
  "connection-timeout",
  "host-gone",
  "host-unknown",
  "improper-addressing",
  "internal-server-error",
  "invalid-from",
  "invalid-namespace",
  "invalid-xml",
  "not-authorized",
  "not-well-formed",
  "policy-violation",
  "remote-connection-failed",
  "reset",
  "resource-constraint",
  "restricted-xml",
  "see-other-host",
  "system-shutdown",
  "undefined-condition",
  "unsupported-encoding",
  "unsupported-feature",
  "unsupported-stanza-type",
  "unsupported-version",
]);

/** The complete RFC 6120 §8.3.3 defined stanza-error condition list
 * (self-contained — the overlap with the stream set is deliberate so this
 * list survives stream-set edits), plus the stream-error conditions so
 * mixed consumers keep matching. Telemetry collapses anything outside this
 * list to "unknown". */
export const XMPP_ERROR_CONDITIONS = new Set([
  ...XMPP_STREAM_ERROR_CONDITIONS,
  "bad-request",
  "conflict",
  "feature-not-implemented",
  "forbidden",
  "gone",
  "internal-server-error",
  "item-not-found",
  "jid-malformed",
  "not-acceptable",
  "not-allowed",
  "not-authorized",
  "policy-violation",
  "recipient-unavailable",
  "redirect",
  "registration-required",
  "remote-server-not-found",
  "remote-server-timeout",
  "resource-constraint",
  "service-unavailable",
  "subscription-required",
  "undefined-condition",
  "unexpected-request",
  // Non-RFC conditions Waddle flows branch on as first-class stanza
  // conditions (see STANZA_CONDITION_PRECONDITION_NOT_MET in the Rust
  // client's error.rs — XEP-0223 publish-options rejections).
  "precondition-not-met",
]);

export interface XmppErrorEvent {
  kind: XmppErrorKind;
  /** Whether the client expects to recover on its own without UI intervention. */
  recoverable: boolean;
  /** Short, human-readable reason for local UI/debugging; sanitize before telemetry. */
  detail: string;
  /** Original error object if one was caught. */
  cause?: unknown;
  /** XMPP stream-error condition when `kind === "stream"`, or the RFC 6120
   * §8.3 stanza-error condition when a server error stanza was received. */
  condition?: string;
  /** RFC 6120 §8.3.2 `type` attribute of a received error stanza. */
  errorType?: XmppStanzaErrorType;
  /** Server-provided `<text/>` of a received error stanza. */
  errorText?: string;
  /** Denied MUC room node only; never the domain, resource, or full room JID. */
  roomLocalpart?: string;
  /** XEP-0198 stream-management extension detail when a stream error carries one. */
  streamManagementError?: {
    kind: "handled-count-too-high";
    h: number;
    sendCount: number;
  };
}

export interface ListRoomMembersOptions {
  /**
   * Canonical room JID discovered from disco/topology. Prefer this over
   * reconstructing `${channelId}@muc.domain` when callers have it.
   */
  roomJid?: string;
}

interface ReplyPreview {
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
  /** #1182: `id` is a fabricated UUID — the live stanza carried no wire
   * identity at all (no client id, no XEP-0359 stanza-id/origin-id), so
   * only content-based reconciliation can match it to its MAM copy. */
  synthesizedId?: true;
  wireIds?: string[];
  /** XEP-0308 correction target: original sender message id/origin-id. */
  correctionTargetId?: string;
  fromJid?: string;
  roomJid: string;
  nick: string;
  body: string;
  createdAt: string;
  /** Provenance of `createdAt` (see `TimestampSource`). */
  createdAtSource: TimestampSource;
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
  /** XEP-0511 */
  linkPreviews?: import("@/lib/chat-ui").LinkPreview[];
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
  /** Waddle call-thread anchor marker (`urn:waddle:call-thread:0`). */
  callThread?: CallThreadAnchor;
  /** Waddle call-thread ended fastening target from XEP-0422 apply-to@id. */
  callThreadEnded?: {
    anchorId: string;
    ended: string;
    duration: string;
  };
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
  /** XEP-0333 `<markable/>` request on the inbound stanza. */
  displayedMarkerRequested?: boolean;
  _reactionTarget?: string;
  _reactionEmojis?: string[];
  _reactionSenderId?: string;
}

/** A direct message received/sent via type:"chat" stanzas */
export interface LiveDmMessage {
  id: string;
  /** XEP-0313 archive UID from the MAM result envelope, when known. */
  archiveId?: string;
  /** #1182: `id` is a fabricated UUID — see `LiveRoomMessage.synthesizedId`. */
  synthesizedId?: true;
  wireIds?: string[];
  /** XEP-0308 correction target: original sender message id/origin-id. */
  correctionTargetId?: string;
  /**
   * XEP-0045 §7.5 (#1256): this is a MUC private message. `peerJid` is
   * the FULL occupant JID (`room@service/nick`) — the conversation
   * identity — and replies MUST address it verbatim; stripping the
   * resource would broadcast to the whole room.
   */
  mucPm?: boolean;
  /**
   * XEP-0280 restamp pass (#1267 item 6): this dispatch exists ONLY to
   * upgrade an already-rendered row's fallback timestamp with the
   * carbon's authoritative forwarded <delay/>. The timeline merge
   * consumes it; unread accounting and notifications MUST skip it —
   * the direct copy already produced those side effects.
   */
  timestampRefreshOnly?: boolean;
  peerJid: string;
  fromJid: string;
  nick: string;
  body: string;
  createdAt: string;
  /** Provenance of `createdAt` (see `TimestampSource`). */
  createdAtSource: TimestampSource;
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
  /** XEP-0511 */
  linkPreviews?: import("@/lib/chat-ui").LinkPreview[];
  extensionAnnotations?: ExtensionAnnotation[];
  extensionBodyFallback?: boolean;
  isSticker?: boolean;
  /** XEP-0461 */
  replyTo?: ReplyPreview;
  /** RFC 6121 / XEP-0201 */
  threadId?: string;
  parentThreadId?: string;
  /** Waddle call-thread anchor marker (`urn:waddle:call-thread:0`). */
  callThread?: CallThreadAnchor;
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
  /** XEP-0333 `<markable/>` request on the inbound stanza. */
  displayedMarkerRequested?: boolean;
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
  /** RFC 3339 instant the reaction stanza was emitted (delay stamp on
   * SM/MAM replays). Absent on live undelayed stanzas. */
  occurredAt?: string;
}

export interface DmConversation {
  peerJid: string;
  peerUsername: string;
  /**
   * XEP-0045 §7.5 (#1256): this is a MUC private-message conversation.
   * `peerJid` is the FULL occupant JID (`room@service/nick`) and
   * `mucPmRoomJid` carries the room for provenance rendering — an
   * occupant nick must never be displayed as if it were an account
   * identity (nick spoofing).
   */
  mucPm?: boolean;
  mucPmRoomJid?: string;
  peerAvatarUrl?: string | null;
  lastMessageBody?: string;
  lastMessageAt?: string;
  unreadCount: number;
  presenceShow?: "available" | "away" | "xa" | "dnd" | "offline";
  /**
   * XEP-0319 idle instant (epoch ms) behind an away/xa presence, for rendering
   * the idle age ("away, idle 20m"). Absent when the peer is interacting.
   */
  presenceIdleSince?: number;
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
  /** RFC 3339 instant the reaction stanza was emitted (delay stamp on
   * SM/MAM replays). Absent on live undelayed stanzas. */
  occurredAt?: string;
}

export interface PresenceUpdateEvent {
  bareJid: string;
  /** The sending resource (full-JID part after `/`), or `""` if bare. Used to aggregate a contact's devices via most-available-wins. */
  resource: string;
  show: "available" | "away" | "xa" | "dnd" | "offline";
  status?: string;
  /**
   * XEP-0319 last-interaction instant as epoch ms, when the presence carried
   * an `<idle since>` (auto-away). Absent on an interacting contact. Lets the
   * receiver render the idle age ("away, idle 20m").
   */
  idleSince?: number;
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
  isGroupDm?: boolean;
  /** Whether this room arrived through the account's XEP-0402 bookmark
   * membership view. A change is a concrete membership-generation signal
   * even when the room's disco metadata itself is unchanged. */
  isBookmarked?: boolean;
  /**
   * XEP-0402 bookmark `autojoin` value. Bookmarked rooms with the
   * attribute omitted are `false` per spec; rooms discovered outside
   * bookmark PubSub nodes may set this to `true` as Waddle's sidebar
   * membership convention.
   */
  autojoin?: boolean;
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

export type RoomCatalogFingerprintField =
  | "spaceId"
  | "autojoin"
  | "isGroupDm"
  | "isBookmarked";

export interface DiscoveredTopology {
  spaces: DiscoveredSpace[];
  rooms: DiscoveredChannel[];
  /** False when room discovery or hydration degraded and `rooms` may be incomplete. */
  roomCatalogComplete: boolean;
  /**
   * Exact authority available for terminal-denial reconciliation. Whole-cache
   * replacement remains gated by `roomCatalogComplete`.
   */
  roomReconciliationAuthority: {
    /** True when successful MUC and bookmark listings make absence authoritative. */
    absentRoomKeysAuthoritative: boolean;
    /**
     * Field-level evidence for present rooms. This lets a successful exact-room
     * bookmark source remain useful without treating unrelated failed sources
     * as authoritative for fields they could have overridden.
     */
    roomFingerprints: Array<{
      roomKey: string;
      fields: RoomCatalogFingerprintField[];
    }>;
  };
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
  /** XEP-0359 stanza id of the originating message, if the server stamped one.
   * Used by the foreground→service-worker dedup signal so a SW push that
   * lands after a foreground notification with the same stanza id does not
   * re-render the same banner. */
  stanzaId?: string;
  /** XEP-0372 */
  mentions?: string[];
  /** XEP-0513 */
  broadcastMention?: "everyone" | "here";
  /** True when the event is a MAM catch-up re-emission (XEP-0313 archive
   * decode) rather than a live arrival. Notification/unread accounting
   * must skip these — the message may already have been seen and counted
   * live, and the server inbox is the authority for offline unread. */
  fromArchive?: boolean;
}
