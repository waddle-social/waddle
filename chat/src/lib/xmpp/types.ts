/** Shared types for the XMPP client layer. */

export interface RemoteParticipant {
  jid: string;
  stream: MediaStream;
}

export interface XmppStatusSnapshot {
  state: "online" | "offline" | "reconnecting" | "error";
  detail: string;
}

export interface LiveRoomMessage {
  id: string;
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
  /** XEP-0446/0447 */
  sharedFile?: SharedFileInfo;
  /** XEP-0449 */
  isSticker?: boolean;
  /** XEP-0513 */
  broadcastMention?: "everyone" | "here";
  /** XEP-0482/0483 */
  callInvite?: CallInviteInfo;
  _reactionTarget?: string;
  _reactionEmojis?: string[];
}

/** A direct message received/sent via type:"chat" stanzas */
export interface LiveDmMessage {
  id: string;
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
  sharedFile?: SharedFileInfo;
  isSticker?: boolean;
  callInvite?: CallInviteInfo;
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
}

export interface CallInviteInfo {
  inviteId: string;
  muji: boolean;
  jingleSid?: string;
  jingleJid?: string;
  externalUri?: string;
  meetingDesc?: string;
  video?: boolean;
}

export interface IncomingCallInviteEvent {
  roomJid: string;
  nick: string;
  invite: CallInviteInfo;
}

export interface MediaJoinSession {
  backend: string;
  roomName: string;
  participantId: string;
  participantName: string;
  mediaType: "audio" | "video";
  serverUrl: string;
  token: string;
  expiresAt: string;
  canPublish: boolean;
  canPublishData: boolean;
  canSubscribe: boolean;
}

export type MujiCallPhase =
  | "idle"
  | "acquiring-media"
  | "dialing"
  | "ringing"
  | "active"
  | "switching"
  | "reconnecting"
  | "ending"
  | "error";

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

export interface DiscoveredWaddle {
  id: string;
  name: string;
  isPublic: boolean;
}

export interface DiscoveredChannel {
  id: string;
  name: string;
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
