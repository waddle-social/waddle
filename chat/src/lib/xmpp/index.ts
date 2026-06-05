export {
  BrowserXmppClient,
  RoomMemberListUnavailableError,
} from "./client";
export type {
  AdminUserEntry,
  AdminUsersPage,
  DmBookmarkItem,
  NotifyMode,
  SetDmNotificationModeResult,
  SetRoomNotificationModeOutcome,
  UserBookmarkItem,
} from "./client";
// Admin V2 — types re-exported for consumption by SpacesPanel,
// ChannelsPanel, and their detail drawers. The `*Ref` /
// `*SetRoleResult` / `*SetAffiliationResult` / `*KickResult` shapes
// are not re-exported because they only ever flow back to the panel
// via the wrapper return type and never need to be named at a
// component boundary.
export type {
  WasmAdminChannelAffiliationEntry,
  WasmAdminChannelListEntry,
  WasmAdminChannelOccupantEntry,
  WasmAdminChannelsListResult,
  WasmAdminSpaceListEntry,
  WasmAdminSpaceMemberEntry,
  WasmAdminSpacesListResult,
} from "./wasm-types";
export {
  barePeerJid,
  jidDomain,
  parseManagedRoomBareJid,
  roomBareJidFor,
} from "./jid";
export type { OutboundFileAttachment } from "./send-types";
export type { InboxEntry } from "./inbox-types";
export type { FeedEntry, FeedPostInput, FeedSourceKind } from "./feed-types";
export type { Story, StoryPostInput, StoryReactionItem, StoryReactionSummary } from "./story-types";
export { aggregateStoryReactions, isStoryActive, normalizeStoryReactions, STORY_REACTIONS_MAX } from "./story-types";
export type { StoryRead, StoryReads } from "./story-reads-types";
export {
  pruneStoryReads,
  STORY_READS_MAX_ENTRIES,
  STORY_READS_PRUNE_MS,
} from "./story-reads-types";
export type {
  Attendee,
  CommunityEvent,
  CommunityEventInput,
  Freq,
  PartStat,
  Rrule,
  Weekday,
} from "./event-types";
export { groupEventsWithRsvps, sortEventsUpcomingFirst } from "./event-types";
export type { UserPepProfile } from "./pep-types";
export type {
  DmChatStateEvent,
  DmConversation,
  DmDisplayedEvent,
  DmReactionEvent,
  LiveDmMessage,
  LiveRoomMessage,
  MessageSearchResult,
  MucAffiliation,
  MucRole,
  OccupantAuthority,
  OccupantHat,
  OccupantPresence,
  PresenceUpdateEvent,
  RoomActivityEvent,
  RoomAuthority,
  RoomHats,
  RoomPresence,
  SessionLifecycleEvent,
  XmppStatusSnapshot,
} from "./types";
