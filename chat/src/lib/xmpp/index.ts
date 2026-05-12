export {
  BrowserXmppClient,
  RoomMemberListUnavailableError,
} from "./client";
export {
  barePeerJid,
  jidDomain,
  parseManagedRoomBareJid,
  roomBareJidFor,
} from "./jid";
export type { OutboundFileAttachment } from "./send-types";
export type { InboxEntry } from "./inbox-types";
export type { UserPepProfile } from "./pep-types";
export type {
  DmChatStateEvent,
  DmConversation,
  DmDisplayedEvent,
  DmReactionEvent,
  LiveDmMessage,
  LiveRoomMessage,
  MessageSearchResult,
  OccupantHat,
  OccupantPresence,
  PresenceUpdateEvent,
  RoomActivityEvent,
  RoomHats,
  RoomPresence,
  SessionLifecycleEvent,
  XmppStatusSnapshot,
} from "./types";
