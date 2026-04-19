export { BrowserXmppClient } from "./client";
export {
  barePeerJid,
  jidDomain,
  parseManagedRoomBareJid,
  roomBareJidFor,
} from "./jid";
export type { OutboundFileAttachment } from "./messaging";
export type { InboxEntry } from "./inbox";
export type { UserPepProfile } from "./pep-publications";
export type {
  ChatStateType,
  DmChatStateEvent,
  DmConversation,
  DmDisplayedEvent,
  DmReactionEvent,
  LiveDmMessage,
  LiveRoomMessage,
  OccupantHat,
  OccupantPresence,
  PresenceUpdateEvent,
  RoomActivityEvent,
  RoomHats,
  RoomPresence,
  SessionLifecycleEvent,
  XmppStatusSnapshot,
} from "./types";
