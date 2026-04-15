/** Shared types for the XMPP client layer. */

export interface RemoteParticipant {
  jid: string;
  stream: MediaStream;
}

export interface XmppStatusSnapshot {
  state: string;
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
  /** XEP-0446/0447 */
  sharedFile?: SharedFileInfo;
  /** XEP-0449 */
  isSticker?: boolean;
  /** XEP-0513 */
  broadcastMention?: "everyone" | "here";
  /** XEP-0482/0483 */
  callInvite?: CallInviteInfo;
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
}

export interface IncomingCallInviteEvent {
  roomJid: string;
  nick: string;
  invite: CallInviteInfo;
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

export type MujiCallEvent =
  | { type: "incoming"; sid: string; peerJid: string; includesAudio: boolean; includesVideo: boolean }
  | { type: "outgoing"; sid: string; peerJid: string }
  | { type: "accepted"; sid: string }
  | { type: "terminated"; sid: string; reason?: string }
  | { type: "connection-state"; sid: string; state: string }
  | { type: "peer-track-added"; sid: string; track: MediaStreamTrack; stream: MediaStream }
  | { type: "peer-track-removed"; sid: string; track: MediaStreamTrack }
  | { type: "error"; detail: string; sid?: string };

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

export interface ChatStateEvent {
  roomJid: string;
  nick: string;
  state: ChatStateType;
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
