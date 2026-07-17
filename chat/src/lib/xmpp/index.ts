export { BrowserXmppClient, isStaleOwnerDisposeError } from "./client";
export type { CatchupConversationFailure, PubsubEvent } from "./client-events";
export type {
  DmBookmarkItem,
  NotifyMode,
  SetDmNotificationModeResult,
  SetRoomNotificationModeOutcome,
  UserBookmarkItem,
} from "./client-pubsub";
export { RoomMemberListUnavailableError } from "./client-muc-admin";
export type { AdminUserEntry, AdminUsersPage } from "./client-muc-admin";
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
  WasmAdminChannelType,
  WasmAdminChannelsListResult,
  WasmAdminSpaceListEntry,
  WasmAdminSpaceMemberEntry,
  WasmAdminSpacesListResult,
} from "./wasm-types";
export {
  bareJidKey,
  barePeerJid,
  dmConversationIdentityKey,
  jidDomain,
  jidDomainOrEmpty,
  jidLocalpart,
  parseManagedRoomBareJid,
  roomBareJidFor,
} from "./jid";
export type { OutboundFileAttachment } from "./send-types";
export type { InboxEntry } from "./inbox-types";
export type { FeedEntry, FeedPostInput, FeedSourceKind } from "./feed-types";
export type { Story, StoryPostInput, StoryReactionItem, StoryReactionSummary } from "./story-types";
export {
  isStoryActive,
  normalizeStoryReactions,
  STORIES_NODE,
  STORY_REACTIONS_MAX,
  STORY_REACTIONS_SUMMARY_NODE,
} from "./story-types";
export type { StoryRead, StoryReads } from "./story-reads-types";
export {
  pruneStoryReads,
  STORY_READS_MAX_ENTRIES,
  STORY_READS_PRUNE_MS,
} from "./story-reads-types";
export type {
  Attendee,
  CalendarDateValue,
  CommunityEvent,
  CommunityEventInput,
  Freq,
  PartStat,
  Rrule,
  Weekday,
} from "./event-types";
export { groupEventsWithRsvps } from "./event-types";
export {
  addDaysToDateString,
  calendarDateKey,
  calendarDateStartMs,
  compareCalendarDateValues,
  dateTimeValue,
  dateValue,
  eventOverlapsRange,
  isEventUpcomingOrOngoing,
  localDateString,
  localDateStringFromMs,
  localDayRange,
  sortEventsForDay,
  sortEventsUpcomingFirst,
} from "./event-calendar";
export type { UserPepProfile } from "./pep-types";
export type { DmConversationScope } from "./reconnect-catchup";
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
