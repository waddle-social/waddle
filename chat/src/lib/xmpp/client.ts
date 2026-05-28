import { withSpan } from "@/lib/telemetry";
import { inferredFileDisposition } from "@/lib/chat-ui";
import type { MemberSummary, UserSearchResult } from "../chat-types";
import type { WaddleSession } from "../server-auth";
import {
  countQueuedMessages,
  enqueueQueuedMessage,
  listQueuedMessages,
  listQueuedRoomMessages,
  removeQueuedMessage,
  type PersistedQueuedDmMessage,
} from "../outbound-queue-store";
import { $callState, applyCallEvent, clearCallState, tearDownActiveCall } from "@/lib/calls/call-store";
import {
  DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS,
  applyDmCallEvent,
  clearDmCallActivities,
} from "@/lib/calls/dm-call-activity";
import { handleCallEventSideEffect } from "@/lib/calls/call-effects";
import {
  applyMucCallPresence,
  clearMucCallParticipants,
} from "@/lib/calls/muc-call-presence";
import { clearAllLiveCallParticipants } from "@/lib/calls/muc-call-live-participants";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import type { CallWireSender } from "@/lib/calls/outbound";
import type { CallEvent } from "@/lib/calls/types";
import { barePeerJid, fullJidIdentityKey, jidDomain, roomBareJidFor } from "./jid";
import type {
  ChatStateEvent,
  ChatStateType,
  DmChatStateEvent,
  DmDisplayedEvent,
  DmReactionEvent,
  DiscoveredTopology,
  ListRoomMembersOptions,
  LiveDmMessage,
  LiveRoomMessage,
  MamHistoryPage,
  MamPageParam,
  MamThreadPageParam,
  MessageSearchResult,
  PresenceUpdateEvent,
  ReactionEvent,
  RoomActivityEvent,
  RoomAuthority,
  RoomHats,
  RoomPresence,
  RosterContact,
  SessionLifecycleEvent,
  XmppErrorEvent,
  XmppStatusSnapshot,
} from "./types";
import { parseMucAffiliation, parseMucRole } from "./types";
import { prepareEncryptedAttachmentUpload } from "./encrypted-attachments";

import { discoverChannels, discoverTopology } from "./discovery";
import { discoverUploadService, uploadFile, type UploadProgress } from "./file-upload";
import { ReconnectCatchup } from "./reconnect-catchup";
import { parseRegisterDeviceResult, type RegisterDeviceResult } from "./push-register-result";
import {
  createLocalStorageResumePersistence,
  type ResumePersistence,
} from "./resume-persistence";
import { clearDmCallJoinCacheForAccount } from "@/lib/calls/dm-call-join-cache";
import { compareTimelineTimestamps } from "../timeline-timestamps";
import {
  discoverExtensionCommands,
  discoverExtensionRoutes,
  fetchExtensionRouteItems,
  invokeExtensionCommand,
  invokeExtensionLaunch,
  submitExtensionCommandForm,
  type DiscoveredExtensionCommand,
  type DiscoveredExtensionRoute,
  type ExtensionCommandAction,
  type ExtensionCommandFormField,
  type ExtensionCommandResult,
  type ExtensionRouteItem,
} from "./extension-commands";
import {
  feedEntryFromWasm,
  type FeedEntry,
  type FeedPostInput,
  type WasmFeedEntry,
} from "./feed-types";
import {
  storyFromWasm,
  type Story,
  type StoryPostInput,
  type WasmStory,
} from "./story-types";
import {
  storyReadsFromWasm,
  storyReadsToWasm,
  type StoryReads,
  type WasmStoryReads,
} from "./story-reads-types";
import {
  eventFromWasm,
  rruleToWasm,
  type CommunityEvent,
  type CommunityEventInput,
  type PartStat,
  type WasmVEvent,
} from "./event-types";
import type { FetchInboxOptions, InboxEntry, InboxResult } from "./inbox-types";
import type { ActivityPublication, MoodPublication, TunePublication, UserPepProfile } from "./pep-types";
import {
  type OutboundFileAttachment,
  type SendDirectMessageOptions,
  type SendGroupMessageOptions,
} from "./send-types";
import {
  avatarDataUrl,
  buildWasmSendOptions,
  dmMessageFromArchived,
  encodeBodyForSend,
  inboxEntryFromWasm,
  mapPresenceShow,
  parsePresenceShow,
  roomMessageFromArchived,
} from "./wasm-message-codecs";
import type {
  WasmAdminChannelRef,
  WasmAdminChannelsAffiliationsResult,
  WasmAdminChannelsKickResult,
  WasmAdminChannelsListResult,
  WasmAdminChannelsOccupantsResult,
  WasmAdminChannelsSetAffiliationResult,
  WasmAdminSpaceRef,
  WasmAdminSpacesListResult,
  WasmAdminSpacesMembersResult,
  WasmAdminSpacesSetRoleResult,
  WasmArchivedMessage,
  WasmAvatar,
  WasmFetchThreadsOptions,
  WasmInboxResult,
  WasmMamPage,
  WasmMdsDisplayedEntry,
  WasmMessage,
  WasmPepProfile,
  WasmPresence,
  WasmRoomMember,
  WasmRosterContact,
  WasmServerVersion,
  WasmThreadsPage,
  WasmUserSearchResult,
  WasmVCard4,
} from "./wasm-types";
import type { VCard4Profile } from "./vcard4-types";

export { dmMessageFromArchived, roomMessageFromArchived } from "./wasm-message-codecs";

function isRoomActivityMessage(message: LiveRoomMessage): boolean {
  return !!message.body && !message.replacesId && !message.retractsId;
}

function roomActivityEventFromMessage(message: LiveRoomMessage): RoomActivityEvent {
  const activity: RoomActivityEvent = { roomJid: message.roomJid, nick: message.nick, body: message.body };
  if (message.stanzaId) activity.stanzaId = message.stanzaId;
  if (message.mentions) activity.mentions = message.mentions;
  if (message.broadcastMention) activity.broadcastMention = message.broadcastMention;
  return activity;
}

function isMamPageComplete(page: WasmMamPage | null | undefined): boolean {
  const compat = page as (WasmMamPage & { complete?: boolean }) | null | undefined;
  return !!(compat?.is_complete ?? compat?.complete);
}

function pageLastArchiveId(page: WasmMamPage | null | undefined): string | undefined {
  const compat = page as (WasmMamPage & { lastArchiveId?: string }) | null | undefined;
  return compat?.last_id ?? compat?.lastArchiveId;
}

function pageFirstArchiveId(page: WasmMamPage | null | undefined): string | undefined {
  const compat = page as (WasmMamPage & { firstArchiveId?: string }) | null | undefined;
  return compat?.first_id ?? compat?.firstArchiveId;
}

function compareTimestamps(left: string, right: string): number {
  return compareTimelineTimestamps(left, right);
}

function pageCrossesSince(page: WasmMamPage | null | undefined, since: string): boolean {
  return (page?.messages ?? []).some((message) => typeof message.timestamp === "string" && compareTimestamps(message.timestamp, since) < 0);
}

function messageSeenIds(message: Pick<LiveDmMessage | LiveRoomMessage, "id" | "wireIds">): string[] {
  return Array.from(new Set([message.id, ...(message.wireIds ?? [])].filter(Boolean)));
}

function rawMessageSeenIds(message: WasmMessage): string[] {
  return Array.from(new Set([
    message.id,
    message.origin_id,
    message.stanza_id,
    ...(message.stanza_ids?.map((stanzaId) => stanzaId.id) ?? []),
  ].filter((value): value is string => !!value)));
}

function shouldSkipCatchupMessage(
  message: Pick<LiveDmMessage | LiveRoomMessage, "createdAt" | "id" | "wireIds">,
  since?: string,
  seenIds?: ReadonlyArray<string>,
): boolean {
  const seen = new Set(seenIds ?? []);
  if (seen.size > 0 && messageSeenIds(message).some((id) => seen.has(id))) return true;
  if (!since) return false;
  const order = compareTimestamps(message.createdAt, since);
  if (order < 0) return true;
  if (order > 0) return false;
  return false;
}

function shouldSkipRawCatchupMessage(
  message: WasmMessage,
  since?: string,
  seenIds?: ReadonlyArray<string>,
): boolean {
  const seen = new Set(seenIds ?? []);
  if (seen.size > 0 && rawMessageSeenIds(message).some((id) => seen.has(id))) return true;
  if (!since || !message.timestamp) return false;
  return compareTimestamps(message.timestamp, since) < 0;
}

const DM_CALL_ACTIVITY_PAGE_SIZE = 100;
const DM_CALL_ACTIVITY_MAX_PAGES = 50;

type DmCallActivityHydrationOptions = { since?: string; pageSize?: number; maxPages?: number };

async function collectRecentMamPages(
  fetchPage: (max: number, pageParam: MamPageParam) => Promise<WasmMamPage | null | undefined>,
  options: DmCallActivityHydrationOptions,
  shouldContinue: () => boolean,
): Promise<WasmMamPage[]> {
  const since = options.since ?? new Date(Date.now() - DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS).toISOString();
  const pageSize = options.pageSize ?? DM_CALL_ACTIVITY_PAGE_SIZE;
  const maxPages = options.maxPages ?? DM_CALL_ACTIVITY_MAX_PAGES;
  const pages: WasmMamPage[] = [];
  let pageParam: MamPageParam = { type: "latest", start: since };
  const seenBefore = new Set<string>();
  for (let pageIndex = 0; pageIndex < maxPages; pageIndex += 1) {
    if (!shouldContinue()) return [];
    const page = await fetchPage(pageSize, pageParam);
    if (!shouldContinue()) return [];
    if (!page) return pages;
    pages.push(page);
    if (isMamPageComplete(page) || pageCrossesSince(page, since)) return pages;
    const firstArchiveId = pageFirstArchiveId(page);
    if (!firstArchiveId || seenBefore.has(firstArchiveId)) return pages;
    seenBefore.add(firstArchiveId);
    pageParam = { type: "before", before: firstArchiveId, start: since };
  }
  return pages;
}

type WasmModule = typeof import("@waddle/xmpp-client-wasm");
type WasmClient = import("@waddle/xmpp-client-wasm").WaddleClient & {
  fetch_personal_history_page?: (max: number, pageParam: MamPageParam) => Promise<WasmMamPage>;
  discover_extension_routes?: () => Promise<unknown>;
  fetch_extension_route_items?: (route: unknown, roomJid: string) => Promise<unknown>;
  admin_users_list?: (
    prefix: string | null,
    pageSize: number | null,
    afterCursor: string | null,
  ) => Promise<AdminUsersPage>;
  is_community_owner?: () => Promise<boolean>;
  admin_spaces_list?: (args: unknown) => Promise<WasmAdminSpacesListResult>;
  admin_spaces_create?: (args: unknown) => Promise<WasmAdminSpaceRef>;
  admin_spaces_update?: (args: unknown) => Promise<WasmAdminSpaceRef>;
  admin_spaces_delete?: (args: unknown) => Promise<boolean>;
  admin_spaces_members?: (args: unknown) => Promise<WasmAdminSpacesMembersResult>;
  admin_spaces_set_role?: (args: unknown) => Promise<WasmAdminSpacesSetRoleResult>;
  admin_channels_list?: (args: unknown) => Promise<WasmAdminChannelsListResult>;
  admin_channels_create?: (args: unknown) => Promise<WasmAdminChannelRef>;
  admin_channels_update?: (args: unknown) => Promise<WasmAdminChannelRef>;
  admin_channels_delete?: (args: unknown) => Promise<boolean>;
  admin_channels_occupants?: (args: unknown) => Promise<WasmAdminChannelsOccupantsResult>;
  admin_channels_affiliations?: (args: unknown) => Promise<WasmAdminChannelsAffiliationsResult>;
  admin_channels_set_affiliation?: (args: unknown) => Promise<WasmAdminChannelsSetAffiliationResult>;
  admin_channels_kick?: (args: unknown) => Promise<WasmAdminChannelsKickResult>;
};

/**
 * Page returned by the V1 admin Users panel back-end. Typed mirror
 * of the `WaddleAdminUsersPage` struct on the wasm side so the chat
 * layer never has to touch raw JSON. `nextCursor` is `null` when
 * the page is the final one.
 */
export interface AdminUserEntry {
  jid: string;
  display_name?: string | null;
  has_owner_hat: boolean;
}
export interface AdminUsersPage {
  entries: AdminUserEntry[];
  next_cursor?: string | null;
}

/** XEP-0492 fallback notification mode — kebab-case to match the wire
 * name produced by the WASM bridge. The chat UI presents these as
 * "All messages", "Mentions only", "Muted" respectively.
 */
export type NotifyMode = "always" | "on-mention" | "never";

/**
 * One XEP-0402 bookmark item surfaced from
 * `WaddleClient.fetchUserBookmarks` / `setRoomNotificationMode`.
 *
 * `notifyMode` is `null` when the bookmark exists but has no
 * `<notify/>` extension yet (a bookmark another client wrote, or a
 * Waddle bookmark we created before this slice). The chat resolves
 * `null` against the conversation-kind default per XEP-0492 §3 via
 * `resolveDefaultNotifyMode`.
 */
export interface UserBookmarkItem {
  jid: string;
  name: string | null;
  autojoin: boolean;
  notifyMode: NotifyMode | null;
}

/** Typed outcome of [[BrowserXmppClient.setRoomNotificationMode]].
 *
 * `node-config-mismatch` separates the recoverable XEP-0060
 * `precondition-not-met` case (a pre-existing PEP node with a
 * different `access_model`) from generic transport / parser failures.
 * Round-8 XEP reviewer P2. */
export type SetRoomNotificationModeOutcome =
  | { kind: "ok"; item: UserBookmarkItem }
  | { kind: "node-config-mismatch" }
  | { kind: "error" };

type CompatEmitter = {
  on?: (event: string, handler: (...args: any[]) => void) => void;
  off?: (event: string, handler: (...args: any[]) => void) => void;
};

type XmppClientInstance = Partial<WasmClient> & CompatEmitter & {
  joinRoom?: (roomJid: string, nick: string) => Promise<void>;
  leaveRoom?: (roomJid: string, nick: string) => Promise<void>;
  set_on_connected?: (cb: () => void) => void;
  set_on_disconnected?: (cb: () => void) => void;
  set_on_error?: (cb: (error: string) => void) => void;
  set_on_message?: (cb: (message: WasmMessage) => void) => void;
  set_on_presence?: (cb: (presence: WasmPresence) => void) => void;
  set_on_message_delivery_acked?: (cb: (id: string) => void) => void;
  set_on_message_delivery_failed?: (cb: (id: string) => void) => void;
  set_on_call?: (cb: (event: CallEvent) => void) => void;
  set_on_session_lifecycle?: (cb: (event: string) => void) => void;
  set_on_mds_displayed?: (cb: (entry: WasmMdsDisplayedEntry) => void) => void;
  get_resume_state?: () => XmppResumeState | null;
  get_resume_state_handle?: () => XmppResumeStateHandle | undefined;
  publish_mds_displayed?: (chatId: string, stanzaId: string, stanzaIdBy: string) => Promise<void>;
  fetch_mds_displayed?: () => Promise<ReadonlyArray<WasmMdsDisplayedEntry>>;
  subscribe_mds_displayed?: () => Promise<void>;
  ensure_push_node?: (
    serviceJid: string,
    appId: string,
  ) => Promise<{ id: string; jid: string; appId: string }>;
  register_web_push_device?: (options: {
    serviceJid: string;
    node: string;
    deviceId: string;
    environment: string;
    providerEndpoint: string;
    providerToken: string;
    providerKeyMaterial: string;
  }) => Promise<{ id: string; node: string; status: "active" | "disabled" }>;
  disable_push_device?: (
    serviceJid: string,
    node: string,
    deviceId: string,
  ) => Promise<{ id: string; node: string; status: "active" | "disabled" }>;
  fetch_user_bookmarks?: () => Promise<UserBookmarkItem[] | null>;
  set_room_notification_mode?: (options: {
    roomJid: string;
    mode: NotifyMode;
    name?: string;
  }) => Promise<SetRoomNotificationModeOutcome>;
};

/**
 * Typed XEP-0490 entry surfaced from the WASM client into the chat
 * layer. Module-private — external consumers can use TypeScript
 * structural typing (`{ chatId, stanzaId, stanzaIdBy }`) on the
 * `setMdsDisplayedHandler` callback parameter without importing
 * this name.
 */
interface MdsDisplayedEntry {
  /** PEP item id = bare JID of the chat (DM contact or MUC room). */
  chatId: string;
  /** XEP-0359 id of the latest displayed message. */
  stanzaId: string;
  /** JID that injected the stanza-id (room for MUC, user's server for DM). */
  stanzaIdBy: string;
}

type XmppResumeState = {
  previd: string;
  inboundH: number;
  outboundH: number;
  resource?: string;
};

type XmppResumeStateHandle = import("@waddle/xmpp-client-wasm").WaddleResumeState;

interface OutboundSendResult {
  id: string | null;
  state: "queued" | "sending";
}

let wasmModulePromise: Promise<WasmModule> | null = null;

function createXmppResource() {
  const randomId = globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
  return `web-${randomId}`;
}

async function loadWasmModule(): Promise<WasmModule> {
  if (!wasmModulePromise) {
    wasmModulePromise = import("@waddle/xmpp-client-wasm").then(async (mod) => {
      if (typeof mod.default === "function") {
        await mod.default();
      }
      // Swap the chat-ui consistent-color implementation over to the
      // spec-conformant SHA-1 hue (XEP-0392 §5.1). The chat-ui module's
      // own DJB2 fallback covers the first-paint window before this
      // runs.
      const { setConsistentColorBackend } = await import("@/lib/chat-ui");
      setConsistentColorBackend(mod.xep0392_consistent_hue);
      return mod;
    });
  }
  return wasmModulePromise;
}

export class RoomMemberListUnavailableError extends Error {
  constructor(message = "Member list is temporarily unavailable.") {
    super(message);
    this.name = "RoomMemberListUnavailableError";
  }
}

export class BrowserXmppClient {
  private session: WaddleSession;
  private get queueScope() { return barePeerJid(this.session.jid); }
  private readonly resource: string;
  private messageHandler: ((message: LiveRoomMessage) => void) | null = null;
  private pinEventHandler: ((event: { roomJid: string; event: import("./wasm-types").WasmPinEvent }) => void) | null = null;
  private directMessageHandler: ((message: LiveDmMessage) => void) | null = null;
  private statusHandler: ((status: XmppStatusSnapshot) => void) | null = null;
  private reactionHandler: ((event: ReactionEvent) => void) | null = null;
  private displayedHandler: ((event: { roomJid: string; nick: string; messageId: string }) => void) | null = null;
  private mdsDisplayedHandler: ((entry: MdsDisplayedEntry) => void) | null = null;
  private chatStateHandler: ((event: ChatStateEvent) => void) | null = null;
  private dmChatStateHandler: ((event: DmChatStateEvent) => void) | null = null;
  private dmReactionHandler: ((event: DmReactionEvent) => void) | null = null;
  private dmDisplayedHandler: ((event: DmDisplayedEvent) => void) | null = null;
  private presenceUpdateHandler: ((event: PresenceUpdateEvent) => void) | null = null;
  private roomMemberJidsByRoom = new Map<string, Record<string, string>>();
  private memberJidHandler: ((nick: string, bareJid: string) => void) | null = null;
  private hatsHandler: ((hats: RoomHats) => void) | null = null;
  private authorityHandler: ((authority: RoomAuthority) => void) | null = null;
  private activityHandler: ((event: RoomActivityEvent) => void) | null = null;
  private inboxPushHandler: ((entry: InboxEntry) => void) | null = null;
  private roomAvatarHandler: ((roomJid: string, hash: string) => void) | null = null;
  private roomDisconnectHandler: (() => void) | null = null;
  private presenceHandler: ((presence: RoomPresence) => void) | null = null;
  private lastSeenHandler: ((nick: string, timestamp: number) => void) | null = null;
  private messageAckHandler: ((messageId: string) => void) | null = null;
  private messageDeliveryFailureHandler: ((messageId: string) => void) | null = null;
  private queuedMessageStatusHandler: ((messageId: string, status: "queued" | "sending") => void) | null = null;
  private sessionLifecycleHandler: ((event: SessionLifecycleEvent) => void) | null = null;
  private xmpp: XmppClientInstance | null = null;
  private connectPromise: Promise<void> | null = null;
  private connected = false;
  private destroying = false;
  private currentRoom: string | null = null;
  private roomSwitchPromise: Promise<void> | null = null;
  private roomSwitchTarget: string | null = null;
  private selfPingTimer: ReturnType<typeof setInterval> | null = null;
  private roomHatsByRoom = new Map<string, RoomHats>();
  private roomAuthorityByRoom = new Map<string, RoomAuthority>();
  private roomPresenceByRoom = new Map<string, RoomPresence>();
  private readonly joinedMucs = new Map<string, Promise<void>>();
  private readonly joinedMucJoinTokens = new Map<string, symbol>();
  private readonly joinedMucReady = new Set<string>();
  private retainedJoinedRoomJids = new Set<string>();
  private autoJoinRoomJids: ReadonlyArray<string> = [];
  private uploadServiceJid: string | null = null;
  private discoveredRoomJids = new Map<string, string>();
  private reconnectStartedAt: number | null = null;
  private reconnectAttempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private resumeState: XmppResumeState | null = null;
  private resumeStateHandle: XmppResumeStateHandle | null = null;
  private directQueueFlushPromise: Promise<void> | null = null;
  private readonly roomQueueFlushes = new Map<string, Promise<void>>();
  private readonly inflightQueuedIds = new Set<string>();
  private readonly pendingSendAt = new Map<string, { at: number; kind: "room" | "dm" }>();
  private readonly messageAckHooks: Array<(id: string, meta: { kind: "room" | "dm"; latencyMs: number }) => void> = [];
  private readonly messageFailHooks: Array<(id: string, meta: { kind: "room" | "dm" }) => void> = [];
  private readonly sessionLifecycleHooks: Array<(event: SessionLifecycleEvent) => void> = [];
  private readonly statusHooks: Array<(status: XmppStatusSnapshot, meta: { reconnectDurationMs?: number }) => void> = [];
  private readonly sendEnqueuedHooks: Array<(info: { kind: "room" | "dm"; reason: string }) => void> = [];
  private readonly queueDepthHooks: Array<(depth: { persisted: number; inflight: number }) => void> = [];
  private readonly errorHooks: Array<(event: XmppErrorEvent) => void> = [];
  private readonly roomJoinWaiters = new Map<string, { resolve: () => void; reject: (error: Error) => void }>();
  private readonly carbonDedupIds = new Set<string>();
  readonly catchup: ReconnectCatchup;
  private readonly resumePersistence: ResumePersistence;
  // Per-resume gate: non-zero while `handleSessionReady` is draining
  // MAM catch-up. Live body messages received during that window are
  // buffered in `pendingDuringResume` and replayed after catch-up
  // finishes, so a live arrival can never advance the catch-up cursor
  // mid-pagination (see XEP-0313 §3.3 "Querying the archive" — server
  // pagination is relative to the cursor we send, and shifting it while
  // a `before=` page is in flight can skip or duplicate messages near
  // the page boundary).
  //
  // `resumeBarrier` coalesces redundant resume triggers. Three event
  // sources can fire `handleSessionReady` for the same xmpp handle:
  // `set_on_session_lifecycle`, `on("session:started")`, and
  // `on("stream:management:resumed")`. Without coalescing, the second
  // trigger reset the live buffer mid-flight and the first's drain
  // silently skipped — losing every message that arrived during the
  // first catch-up. Now: the first trigger owns the barrier, the
  // others bail out at the gate.
  //
  // The barrier is keyed to the xmpp handle that owns it. A full
  // reconnect produces a new handle, and the new handle must be
  // allowed to start its own catch-up even if the old handle's
  // barrier is still pending — otherwise `connect()` for the new
  // handle hangs until its 15s timeout fires (the Wi-Fi-to-cellular
  // mobile case the adversarial reviewer flagged).
  //
  // `pendingDuringResume` is a typed sentinel — null means "not
  // buffering, dispatch live messages immediately"; an array means
  // "buffering, push live arrivals here for replay when the barrier
  // resolves." Both fields are written atomically together so a
  // re-entrant `handleSessionReady` (synchronous WASM callback re-
  // entry) cannot observe one set without the other.
  private resumeBarrier: { xmpp: XmppClientInstance; promise: Promise<void> } | null = null;
  private pendingDuringResume: Array<WasmMessage & { carbon?: { sent?: boolean; received?: boolean }; inboxPush?: InboxEntry; _fromCarbon?: boolean }> | null = null;

  constructor(session: WaddleSession, persistence?: ResumePersistence) {
    this.session = session;
    // Per-account so a logout/login on the same browser doesn't mix
    // cursors. `session.jid` is the bare JID — already unique per
    // identity — and matches the `accountKey` used by the outbound
    // queue store.
    this.resumePersistence = persistence ?? createLocalStorageResumePersistence(session.jid);
    this.catchup = new ReconnectCatchup(this.resumePersistence);
    // Restore any XEP-0198 resume state persisted by a prior tab
    // session. If a `resumeStateHandle` is also recovered via the
    // WASM client (live, same JS context), that takes precedence in
    // `doConnect`. The POD `resumeState` is the only piece that
    // survives a full page reload, so hydrate it eagerly.
    this.resumeState = this.resumePersistence.consumeSm();
    this.resource = this.resumeState?.resource || createXmppResource();
    this.retainedJoinedRoomJids = new Set(this.resumePersistence.loadJoinedRooms());
  }

  /** Full JID (`bare/resource`) for this session. Needed by the
   * call layer when constructing Jingle session-initiate / accept,
   * which must address the peer's full JID and stamp our own. */
  get fullJid(): string { return `${this.session.jid}/${this.resource}`; }
  /** Bare JID for this session. */
  get bareJid(): string { return this.session.jid; }

  private isCurrentXmpp(xmpp: XmppClientInstance): boolean {
    return this.xmpp === xmpp && !this.destroying;
  }

  private isCurrentConnectedXmpp(xmpp: XmppClientInstance, sessionJid: string): boolean {
    return this.xmpp === xmpp && this.connected && !this.destroying && this.session.jid === sessionJid;
  }

  private rejectRoomJoinWaiters(error: Error): void {
    for (const waiter of this.roomJoinWaiters.values()) {
      waiter.reject(error);
    }
    this.roomJoinWaiters.clear();
  }

  private ownFullJidCandidates(): Set<string> {
    return new Set([
      fullJidIdentityKey(this.fullJid),
      fullJidIdentityKey(`${barePeerJid(this.session.jid)}/${this.resource}`),
    ]);
  }

  private isOwnMucSelfPresence(presence: { muc_jid?: string | null }): boolean {
    const mucJid = fullJidIdentityKey(presence.muc_jid);
    return !!mucJid && this.ownFullJidCandidates().has(mucJid);
  }

  private isOwnAvailableMucSelfPresence(presence: {
    muc_jid?: string | null;
    presence_type?: string;
  }): boolean {
    return this.isOwnMucSelfPresence(presence) && presence.presence_type !== "unavailable";
  }

  private revokeMucReadiness(roomJid: string, options: { keepPendingJoin?: boolean } = {}): void {
    const normalized = roomJid.trim().toLowerCase();
    if (!options.keepPendingJoin) {
      this.joinedMucs.delete(roomJid);
      this.joinedMucJoinTokens.delete(roomJid);
    }
    this.joinedMucReady.delete(roomJid);
    if (normalized && normalized !== roomJid) {
      if (!options.keepPendingJoin) {
        this.joinedMucs.delete(normalized);
        this.joinedMucJoinTokens.delete(normalized);
      }
      this.joinedMucReady.delete(normalized);
    }
  }

  setMessageHandler(h: (message: LiveRoomMessage) => void) { this.messageHandler = h; }
  /** #414: receive `<pin-event/>` system messages from a room. */
  setPinEventHandler(h: (event: { roomJid: string; event: import("./wasm-types").WasmPinEvent }) => void) { this.pinEventHandler = h; }
  setDirectMessageHandler(h: (message: LiveDmMessage) => void) { this.directMessageHandler = h; }
  setStatusHandler(h: (status: XmppStatusSnapshot) => void) { this.statusHandler = h; }
  setChatStateHandler(h: (event: ChatStateEvent) => void) { this.chatStateHandler = h; }
  setDmChatStateHandler(h: (event: DmChatStateEvent) => void) { this.dmChatStateHandler = h; }
  setReactionHandler(h: (event: ReactionEvent) => void) { this.reactionHandler = h; }
  setDmReactionHandler(h: (event: DmReactionEvent) => void) { this.dmReactionHandler = h; }
  setDisplayedHandler(h: (event: { roomJid: string; nick: string; messageId: string }) => void) { this.displayedHandler = h; }
  setDmDisplayedHandler(h: (event: DmDisplayedEvent) => void) { this.dmDisplayedHandler = h; }
  setPresenceUpdateHandler(h: (event: PresenceUpdateEvent) => void) { this.presenceUpdateHandler = h; }
  setMemberJidHandler(h: (nick: string, bareJid: string) => void) { this.memberJidHandler = h; }
  setHatsHandler(h: (hats: RoomHats) => void) { this.hatsHandler = h; }
  setAuthorityHandler(h: (authority: RoomAuthority) => void) { this.authorityHandler = h; }
  setActivityHandler(h: (event: RoomActivityEvent) => void) { this.activityHandler = h; }
  setInboxPushHandler(h: (entry: InboxEntry) => void) { this.inboxPushHandler = h; }
  setRoomAvatarHandler(h: (roomJid: string, hash: string) => void) { this.roomAvatarHandler = h; }
  setRoomDisconnectHandler(h: () => void) { this.roomDisconnectHandler = h; }
  setPresenceHandler(h: (presence: RoomPresence) => void) { this.presenceHandler = h; }
  setLastSeenHandler(h: (nick: string, timestamp: number) => void) { this.lastSeenHandler = h; }
  setMessageAckHandler(h: (messageId: string) => void) { this.messageAckHandler = h; }
  setMessageDeliveryFailureHandler(h: (messageId: string) => void) { this.messageDeliveryFailureHandler = h; }
  setQueuedMessageStatusHandler(h: (messageId: string, status: "queued" | "sending") => void) { this.queuedMessageStatusHandler = h; }
  setSessionLifecycleHandler(h: (event: SessionLifecycleEvent) => void) { this.sessionLifecycleHandler = h; }

  onMessageAcked(hook: (id: string, meta: { kind: "room" | "dm"; latencyMs: number }) => void) { this.messageAckHooks.push(hook); }
  onMessageDeliveryFailed(hook: (id: string, meta: { kind: "room" | "dm" }) => void) { this.messageFailHooks.push(hook); }
  onSessionLifecycle(hook: (event: SessionLifecycleEvent) => void) { this.sessionLifecycleHooks.push(hook); }
  onStatus(hook: (status: XmppStatusSnapshot, meta: { reconnectDurationMs?: number }) => void) { this.statusHooks.push(hook); }
  onSendEnqueued(hook: (info: { kind: "room" | "dm"; reason: string }) => void) { this.sendEnqueuedHooks.push(hook); }
  onQueueDepthChange(hook: (depth: { persisted: number; inflight: number }) => void) { this.queueDepthHooks.push(hook); }
  onError(hook: (event: XmppErrorEvent) => void) { this.errorHooks.push(hook); }

  private fireHook<Args extends unknown[]>(hooks: Array<(...args: Args) => void>, ...args: Args) {
    for (const hook of hooks) {
      try { hook(...args); } catch (error) { console.error("xmpp telemetry hook threw", error); }
    }
  }

  private emitError(event: XmppErrorEvent) { this.fireHook(this.errorHooks, event); }

  private updateReconnectTimer(snap: XmppStatusSnapshot): { reconnectDurationMs?: number } {
    if (snap.state === "reconnecting") {
      if (this.reconnectStartedAt === null) this.reconnectStartedAt = performance.now();
      return {};
    }
    if (this.reconnectStartedAt === null) return {};
    if (snap.state === "online") {
      const durationMs = performance.now() - this.reconnectStartedAt;
      this.reconnectStartedAt = null;
      return { reconnectDurationMs: durationMs };
    }
    this.reconnectStartedAt = null;
    return {};
  }

  private emitStatus(snapshot: XmppStatusSnapshot) {
    this.statusHandler?.(snapshot);
    const meta = this.updateReconnectTimer(snapshot);
    this.fireHook(this.statusHooks, snapshot, meta);
  }

  private emitSessionLifecycle(event: SessionLifecycleEvent) {
    this.sessionLifecycleHandler?.(event);
    this.fireHook(this.sessionLifecycleHooks, event);
  }

  private emitQueueDepth() {
    this.fireHook(this.queueDepthHooks, {
      persisted: countQueuedMessages(this.queueScope),
      inflight: this.inflightQueuedIds.size,
    });
  }

  private recordPendingSend(id: string | null, kind: "room" | "dm") {
    if (!id) return;
    this.pendingSendAt.set(id, { at: performance.now(), kind });
  }

  private clearReconnectTimer() {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private clearResumeState() {
    this.resumeState = null;
    this.setResumeStateHandle(null);
    this.resumePersistence.clearSm();
    this.resumePersistence.clearJoinedRooms();
    // Only called from the `destroying` path — i.e. intentional
    // logout / shutdown. Drop the catch-up cursors too so a future
    // login on the same account (same browser) doesn't replay
    // ancient MAM history. (Transient disconnects do NOT go through
    // this method; they intentionally keep cursors so the next
    // resume can fill the gap.)
    this.catchup.reset();
  }

  persistResumeStateForPageHide(): void {
    const xmpp = this.xmpp;
    const state = xmpp?.get_resume_state?.() ?? this.resumeState;
    this.resumePersistence.preparePagehideHandoff();
    if (state) {
      const snapshot = { ...state, resource: this.resource };
      this.resumeState = snapshot;
      this.resumePersistence.saveSm(snapshot);
    }
    this.persistRetainedJoinedRooms();
  }

  private setResumeStateHandle(handle: XmppResumeStateHandle | null | undefined) {
    if (this.resumeStateHandle && this.resumeStateHandle !== handle) {
      this.disposeResumeStateHandle(this.resumeStateHandle);
    }
    this.resumeStateHandle = handle ?? null;
  }

  private disposeResumeStateHandle(handle: XmppResumeStateHandle) {
    try {
      handle.free();
    } catch {}
  }

  private scheduleReconnect() {
    if (this.destroying || this.reconnectTimer) return;
    const delay = Math.min(2000 * (2 ** this.reconnectAttempt), 60000);
    this.reconnectAttempt += 1;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      void this.connect().catch(() => undefined);
    }, delay);
  }

  private async enableCarbons(xmpp: XmppClientInstance & { enableCarbons?: () => Promise<void> }) {
    if (xmpp.enableCarbons) {
      try { await xmpp.enableCarbons(); } catch {}
      return;
    }
    if (!xmpp.send_raw_iq) return;
    try { await xmpp.send_raw_iq(`<iq type="set" id="${crypto.randomUUID()}"><enable xmlns="urn:xmpp:carbons:2"/></iq>`); } catch {}
  }

  private async doConnect(): Promise<void> {
    const mod = await loadWasmModule();
    const config = new mod.WaddleConfig(
      this.session.xmpp_websocket_url,
      this.session.jid,
      this.session.session_id,
      this.resource,
    );
    if (this.resumeStateHandle && typeof (config as any).with_resume_state_handle === "function") {
      const handle = this.resumeStateHandle;
      (config as any).with_resume_state_handle(handle);
      this.clearResumeState();
    } else if (this.resumeState) {
      (config as any).with_resume_state?.(
        this.resumeState.previd,
        this.resumeState.inboundH,
        this.resumeState.outboundH,
      );
      this.resumePersistence.clearSm();
      this.resumeState = null;
    }
    const xmpp = new mod.WaddleClient(config) as unknown as XmppClientInstance;
    this.xmpp = xmpp;
    this.wireEvents(xmpp);
    await xmpp.connect?.();
  }

  private onceConnected: (() => void) | null = null;
  private onceConnectFailed: ((error: Error) => void) | null = null;

  async connect(): Promise<void> {
    if (this.xmpp && this.connected) return;
    if (this.connectPromise) return this.connectPromise;
    this.destroying = false;
    this.clearReconnectTimer();
    this.connectPromise = withSpan(
      "xmpp.connect",
      { "waddle.xmpp.jid": this.session.jid, "waddle.xmpp.transport": "websocket" },
      () => new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => {
          this.connectPromise = null;
          reject(new Error("Reconnection timed out"));
        }, 15000);
        const done = (fn: () => void) => {
          clearTimeout(timeout);
          this.connectPromise = null;
          fn();
        };
        this.onceConnected = () => done(resolve);
        this.onceConnectFailed = (error) => done(() => reject(error));
        void this.doConnect().catch((error) => {
          if (this.onceConnectFailed) {
            const fail = this.onceConnectFailed;
            this.onceConnectFailed = null;
            fail(error instanceof Error ? error : new Error(String(error)));
          }
        });
      }),
    );
    return this.connectPromise;
  }

  async disconnect() {
    this.destroying = true;
    this.clearReconnectTimer();
    this.clearResumeState();
    const xmpp = this.xmpp;
    const joinedRooms = [...this.joinedMucs.keys()];
    this.stopSelfPing();
    this.connected = false;
    this.connectPromise = null;
    this.currentRoom = null;
    this.roomSwitchPromise = null;
    this.roomSwitchTarget = null;
    this.rejectRoomJoinWaiters(new Error("XMPP disconnected while joining a room"));
    this.uploadServiceJid = null;
    this.clearRoomPresenceCaches();
    this.retainedJoinedRoomJids.clear();
    this.joinedMucs.clear();
    this.joinedMucJoinTokens.clear();
    this.joinedMucReady.clear();
    clearDmCallJoinCacheForAccount(this.session.jid);
    clearDmCallActivities();
    clearMucCallParticipants();
    clearAllLiveCallParticipants();
    // Best-effort hangup: if we're in a call when the user logs out
    // we want the peer to see session-terminate before the stream
    // closes. `tearDownActiveCall` handles every phase and clears
    // `$callState`.
    await tearDownActiveCall(xmpp as unknown as CallWireSender | null, "success");
    this.xmpp = null;
    if (xmpp?.leave_room) {
      for (const roomJid of joinedRooms) {
        try {
          await xmpp.leave_room(roomJid, this.session.username);
        } catch {}
      }
    }
    await xmpp?.disconnect?.();
    this.emitStatus({ state: "offline", detail: "Disconnected" });
  }

  private roomJidForChannel(channelId: string): string {
    return this.discoveredRoomJids.get(channelId) ?? roomBareJidFor(this.session, channelId);
  }

  rememberRoomJidForChannel(channelId: string, roomJid: string): void {
    const normalizedRoomJid = barePeerJid(roomJid);
    if (!channelId || !normalizedRoomJid) return;
    this.discoveredRoomJids.set(channelId, normalizedRoomJid);
  }

  private waitForRoomSelfPresence(roomJid: string, nick: string): Promise<void> {
    const fullJid = `${roomJid}/${nick}`;
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.roomJoinWaiters.delete(fullJid);
        const detail = `Timed out waiting for self-presence in ${roomJid}`;
        this.emitError({ kind: "connect-timeout", recoverable: true, detail });
        reject(new Error("Channel presence did not finish syncing. Try again in a moment."));
      }, 4000);
      this.roomJoinWaiters.set(fullJid, {
        resolve: () => {
          clearTimeout(timeout);
          this.roomJoinWaiters.delete(fullJid);
          resolve();
        },
        reject: (error) => {
          clearTimeout(timeout);
          this.roomJoinWaiters.delete(fullJid);
          reject(error);
        },
      });
    });
  }

  private clearRoomPresenceCaches(): void {
    this.roomHatsByRoom.clear();
    this.roomAuthorityByRoom.clear();
    this.roomPresenceByRoom.clear();
    this.roomMemberJidsByRoom.clear();
  }

  private hatsFor(roomJid: string): RoomHats {
    let cached = this.roomHatsByRoom.get(roomJid);
    if (!cached) {
      cached = {};
      this.roomHatsByRoom.set(roomJid, cached);
    }
    return cached;
  }

  private authorityFor(roomJid: string): RoomAuthority {
    let cached = this.roomAuthorityByRoom.get(roomJid);
    if (!cached) {
      cached = {};
      this.roomAuthorityByRoom.set(roomJid, cached);
    }
    return cached;
  }

  private presenceFor(roomJid: string): RoomPresence {
    let cached = this.roomPresenceByRoom.get(roomJid);
    if (!cached) {
      cached = {};
      this.roomPresenceByRoom.set(roomJid, cached);
    }
    return cached;
  }

  private memberJidsFor(roomJid: string): Record<string, string> {
    let cached = this.roomMemberJidsByRoom.get(roomJid);
    if (!cached) {
      cached = {};
      this.roomMemberJidsByRoom.set(roomJid, cached);
    }
    return cached;
  }

  private dispatchFocusedRoomHandlers(): void {
    const roomJid = this.currentRoom;
    if (!roomJid) {
      this.hatsHandler?.({});
      this.authorityHandler?.({});
      this.presenceHandler?.({});
      return;
    }
    this.hatsHandler?.({ ...(this.roomHatsByRoom.get(roomJid) ?? {}) });
    this.authorityHandler?.({ ...(this.roomAuthorityByRoom.get(roomJid) ?? {}) });
    this.presenceHandler?.({ ...(this.roomPresenceByRoom.get(roomJid) ?? {}) });
    const memberJids = this.roomMemberJidsByRoom.get(roomJid) ?? {};
    for (const [nick, bare] of Object.entries(memberJids)) {
      this.memberJidHandler?.(nick, bare);
    }
  }

  async ensureJoined(roomJid: string): Promise<void> {
    if (this.joinedMucReady.has(roomJid)) return;
    const existing = this.joinedMucs.get(roomJid);
    if (existing) {
      const existingToken = this.joinedMucJoinTokens.get(roomJid);
      await existing;
      if (existingToken && this.joinedMucJoinTokens.get(roomJid) !== existingToken) {
        return;
      }
      if (!existingToken && !this.joinedMucs.has(roomJid)) return;
      this.joinedMucReady.add(roomJid);
      this.rememberJoinedRoom(roomJid);
      return;
    }
    const promise = this.performMucJoin(roomJid);
    const joinToken = Symbol(roomJid);
    this.joinedMucs.set(roomJid, promise);
    this.joinedMucJoinTokens.set(roomJid, joinToken);
    try {
      await promise;
      if (this.joinedMucJoinTokens.get(roomJid) === joinToken) {
        this.joinedMucReady.add(roomJid);
        this.rememberJoinedRoom(roomJid);
      }
    } catch (err) {
      if (this.joinedMucJoinTokens.get(roomJid) === joinToken) {
        this.joinedMucs.delete(roomJid);
        this.joinedMucJoinTokens.delete(roomJid);
        this.joinedMucReady.delete(roomJid);
      }
      throw err;
    }
  }

  private rememberJoinedRoom(roomJid: string): void {
    const normalized = roomJid.split("/")[0]?.trim().toLowerCase() ?? "";
    if (!normalized) return;
    const sizeBefore = this.retainedJoinedRoomJids.size;
    this.retainedJoinedRoomJids.add(normalized);
    if (this.retainedJoinedRoomJids.size !== sizeBefore) this.persistRetainedJoinedRooms();
  }

  private persistRetainedJoinedRooms(): void {
    this.resumePersistence.saveJoinedRooms([...this.retainedJoinedRoomJids]);
  }

  private async performMucJoin(roomJid: string): Promise<void> {
    const xmpp = this.xmpp;
    if (!xmpp) throw new Error("XMPP session is not ready");
    if (!xmpp.join_room && !xmpp.joinRoom) {
      throw new Error("XMPP client missing join_room binding");
    }
    const ready = this.waitForRoomSelfPresence(roomJid, this.session.username);
    if (xmpp.join_room) {
      await Promise.allSettled([xmpp.join_room(roomJid, this.session.username)]);
    } else if (xmpp.joinRoom) {
      await xmpp.joinRoom(roomJid, this.session.username);
    }
    if (!this.isCurrentXmpp(xmpp)) {
      await ready.catch(() => undefined);
      return;
    }
    await ready;
  }

  async fanOutAutoJoin(roomJids: ReadonlyArray<string>, concurrency = 6): Promise<void> {
    if (!this.xmpp) return;
    const queue = [...new Set(roomJids.filter(Boolean))];
    const workers = Array.from({ length: Math.max(1, concurrency) }, async () => {
      while (queue.length > 0) {
        const roomJid = queue.shift();
        if (!roomJid) continue;
        try {
          await this.ensureJoined(roomJid);
        } catch {
          // Failed joins drop their tracker entry inside ensureJoined.
          // The next topology refresh or reconnect replay can retry.
        }
      }
    });
    await Promise.allSettled(workers);
  }

  async switchRoom(_spaceId: string, channelId: string) {
    await this.connect();
    const nextRoom = this.roomJidForChannel(channelId);
    if (this.roomSwitchPromise) {
      if (this.roomSwitchTarget === nextRoom) return this.roomSwitchPromise;
      await this.roomSwitchPromise.catch(() => undefined);
    }
    if (this.currentRoom === nextRoom) {
      await this.ensureJoined(nextRoom);
      this.dispatchFocusedRoomHandlers();
      await this.flushQueuedRoomMessages(nextRoom);
      return;
    }
    const promise = this.performRoomSwitch(nextRoom);
    this.roomSwitchPromise = promise;
    this.roomSwitchTarget = nextRoom;
    try {
      await promise;
    } finally {
      if (this.roomSwitchPromise === promise) {
        this.roomSwitchPromise = null;
        this.roomSwitchTarget = null;
      }
    }
  }

  private async performRoomSwitch(nextRoom: string) {
    const xmpp = this.xmpp;
    if (!xmpp) return;
    this.currentRoom = nextRoom;
    this.dispatchFocusedRoomHandlers();
    try {
      await this.ensureJoined(nextRoom);
    } catch (err) {
      if (!this.isCurrentXmpp(xmpp)) return;
      throw err;
    }
    if (!this.isCurrentXmpp(xmpp)) return;
    this.dispatchFocusedRoomHandlers();
    this.startSelfPing();
    await this.flushQueuedRoomMessages(nextRoom);
  }

  private browserOffline(): boolean {
    return typeof navigator !== "undefined" && navigator.onLine === false;
  }

  private canUseConnectedSession(): boolean {
    return !!this.xmpp && this.connected && !this.destroying && !this.browserOffline();
  }

  private roomIsReady(roomJid: string): boolean {
    return this.canUseConnectedSession() && this.currentRoom === roomJid && this.joinedMucReady.has(roomJid);
  }

  private enqueueReason(): string {
    if (this.browserOffline()) return "offline";
    if (this.destroying) return "destroying";
    if (!this.xmpp) return "no-client";
    if (!this.connected) return "reconnecting";
    return "not-ready";
  }

  private noteQueuedMessage() {
    const queueCount = countQueuedMessages(this.queueScope);
    this.emitStatus({
      state: this.browserOffline() ? "offline" : "reconnecting",
      detail: queueCount === 1 ? "Message queued until the connection returns" : `${queueCount} messages queued until the connection returns`,
    });
  }

  private queueRoomMessage(roomJid: string, body: string, opts: SendGroupMessageOptions): OutboundSendResult {
    const queuedId = opts.id ?? crypto.randomUUID();
    enqueueQueuedMessage(this.queueScope, {
      kind: "room",
      id: queuedId,
      createdAt: new Date().toISOString(),
      roomJid,
      body,
      ...(opts.markup?.length ? { markup: opts.markup } : {}),
      ...(opts.references?.length ? { references: opts.references } : {}),
      ...(opts.mentionJidsByNick ? { mentionJidsByNick: opts.mentionJidsByNick } : {}),
      ...(opts.files?.length ? { files: opts.files } : {}),
      ...(opts.replyTo ? { replyTo: opts.replyTo } : {}),
      ...(opts.threadId ? { threadId: opts.threadId } : {}),
      ...(opts.parentThreadId ? { parentThreadId: opts.parentThreadId } : {}),
      ...(opts.threadCreate ? { threadCreate: opts.threadCreate } : {}),
      ...(opts.threadReply ? { threadReply: opts.threadReply } : {}),
    });
    this.queuedMessageStatusHandler?.(queuedId, "queued");
    this.noteQueuedMessage();
    this.fireHook(this.sendEnqueuedHooks, { kind: "room", reason: this.enqueueReason() });
    this.emitQueueDepth();
    return { id: queuedId, state: "queued" };
  }

  private queueDirectMessage(peerJid: string, body: string, opts: SendDirectMessageOptions): OutboundSendResult {
    const queuedId = opts.id ?? crypto.randomUUID();
    enqueueQueuedMessage(this.queueScope, {
      kind: "dm",
      id: queuedId,
      createdAt: new Date().toISOString(),
      peerJid: barePeerJid(peerJid),
      body,
      ...(opts.markup?.length ? { markup: opts.markup } : {}),
      ...(opts.references?.length ? { references: opts.references } : {}),
      ...(opts.files?.length ? { files: opts.files } : {}),
      ...(opts.replyTo ? { replyTo: opts.replyTo } : {}),
      ...(opts.threadId ? { threadId: opts.threadId } : {}),
      ...(opts.parentThreadId ? { parentThreadId: opts.parentThreadId } : {}),
    });
    this.queuedMessageStatusHandler?.(queuedId, "queued");
    this.noteQueuedMessage();
    this.fireHook(this.sendEnqueuedHooks, { kind: "dm", reason: this.enqueueReason() });
    this.emitQueueDepth();
    return { id: queuedId, state: "queued" };
  }

  private async compatSendGroupMessage(xmpp: XmppClientInstance, roomJid: string, body: string, opts: SendGroupMessageOptions): Promise<string | null> {
    const { effectiveBody, replyFallbackLength, rebasedMarkup, rebasedReferences } = encodeBodyForSend(body, opts.replyTo, opts.markup, opts.references);
    const wasmOpts = buildWasmSendOptions({ ...opts, markup: rebasedMarkup, references: rebasedReferences }, replyFallbackLength);
    if (xmpp.send_groupchat_message) return await xmpp.send_groupchat_message(roomJid, effectiveBody, wasmOpts) as string;
    throw new Error("XMPP session is not ready");
  }

  private async compatSendDirectMessage(xmpp: XmppClientInstance, peerJid: string, body: string, opts: SendDirectMessageOptions): Promise<string | null> {
    const { effectiveBody, replyFallbackLength, rebasedMarkup, rebasedReferences } = encodeBodyForSend(body, opts.replyTo, opts.markup, opts.references);
    const wasmOpts = buildWasmSendOptions({ ...opts, markup: rebasedMarkup, references: rebasedReferences }, replyFallbackLength);
    if (xmpp.send_chat_message) return await xmpp.send_chat_message(peerJid, effectiveBody, wasmOpts) as string;
    throw new Error("XMPP session is not ready");
  }

  // XEP-0201 §3: chat-states, displayed markers, reactions, retractions, and
  // corrections that relate to a threaded message SHOULD repeat the original
  // `<thread/>`. The wasm bindings accept thread id + optional parent so the
  // related stanzas route to the same conversation context on receivers.
  private async compatSendChatState(xmpp: XmppClientInstance, to: string, type: "chat" | "groupchat", state: ChatStateType, thread?: { id: string; parent?: string }) {
    if (xmpp.send_chat_state) return xmpp.send_chat_state(to, type, state, thread?.id, thread?.parent);
    throw new Error("XMPP session is not ready");
  }

  private async compatSendDisplayed(xmpp: XmppClientInstance, to: string, type: "chat" | "groupchat", id: string, thread?: { id: string; parent?: string }) {
    if (xmpp.send_displayed) return xmpp.send_displayed(to, type, id, thread?.id, thread?.parent);
    throw new Error("XMPP session is not ready");
  }

  private async compatSendReaction(xmpp: XmppClientInstance, to: string, type: "chat" | "groupchat", id: string, emojis: string[], thread?: { id: string; parent?: string }) {
    if (xmpp.send_reaction) return xmpp.send_reaction(to, type, id, emojis, thread?.id, thread?.parent);
    throw new Error("XMPP session is not ready");
  }

  private async compatSendRetraction(xmpp: XmppClientInstance, to: string, type: "chat" | "groupchat", id: string, thread?: { id: string; parent?: string }) {
    if (xmpp.send_retraction) return xmpp.send_retraction(to, type, id, thread?.id, thread?.parent);
    throw new Error("XMPP session is not ready");
  }

  private async compatSendModeration(xmpp: XmppClientInstance, roomJid: string, id: string, reason?: string) {
    if (xmpp.send_moderation) return xmpp.send_moderation(roomJid, "groupchat", id, reason);
    throw new Error("XMPP session is not ready");
  }

  private async compatSendCorrection(xmpp: XmppClientInstance, to: string, type: "chat" | "groupchat", body: string, replacesId: string, opts?: SendGroupMessageOptions | SendDirectMessageOptions): Promise<string | null> {
    const { effectiveBody, replyFallbackLength, rebasedMarkup, rebasedReferences } = encodeBodyForSend(body, opts?.replyTo, opts?.markup, opts?.references);
    const wasmOpts = buildWasmSendOptions({ ...(opts ?? {}), markup: rebasedMarkup, references: rebasedReferences }, replyFallbackLength);
    if (xmpp.send_correction) return await xmpp.send_correction(to, type, effectiveBody, replacesId, wasmOpts) as string;
    throw new Error("XMPP session is not ready");
  }

  private async requireConnectedXmpp(): Promise<XmppClientInstance> {
    await this.connect();
    if (!this.xmpp || !this.connected || this.destroying) throw new Error("XMPP session is not ready");
    return this.xmpp;
  }

  private async requireJoinedRoom(spaceId: string, channelId: string): Promise<{ xmpp: XmppClientInstance; roomJid: string }> {
    const roomJid = this.roomJidForChannel(channelId);
    await this.switchRoom(spaceId, channelId);
    if (!this.xmpp || !this.connected || this.destroying || this.currentRoom !== roomJid) throw new Error(`Room is not ready: ${roomJid}`);
    return { xmpp: this.xmpp, roomJid };
  }

  private async flushQueuedDirectMessages() {
    if (this.directQueueFlushPromise) return this.directQueueFlushPromise;
    if (!this.canUseConnectedSession() || !this.xmpp) return;
    const promise = (async () => {
      const entries = listQueuedMessages(this.queueScope).filter((entry): entry is PersistedQueuedDmMessage => entry.kind === "dm");
      for (const entry of entries) {
        if (!this.canUseConnectedSession() || !this.xmpp) break;
        if (this.inflightQueuedIds.has(entry.id)) continue;
        this.queuedMessageStatusHandler?.(entry.id, "sending");
        const messageId = await this.compatSendDirectMessage(this.xmpp, barePeerJid(entry.peerJid), entry.body, { ...(entry.markup?.length ? { markup: entry.markup } : {}), ...(entry.references?.length ? { references: entry.references } : {}), ...(entry.files?.length ? { files: entry.files } : {}), ...(entry.replyTo ? { replyTo: entry.replyTo } : {}), ...(entry.threadId ? { threadId: entry.threadId } : {}), ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}), id: entry.id });
        if (messageId) { this.inflightQueuedIds.add(entry.id); this.recordPendingSend(entry.id, "dm"); }
      }
    })();
    this.directQueueFlushPromise = promise.finally(() => { if (this.directQueueFlushPromise === promise) this.directQueueFlushPromise = null; });
    return this.directQueueFlushPromise;
  }

  private async flushQueuedRoomMessages(roomJid: string) {
    const inflight = this.roomQueueFlushes.get(roomJid);
    if (inflight) return inflight;
    if (!this.roomIsReady(roomJid) || !this.xmpp) return;
    const promise = (async () => {
      const entries = listQueuedRoomMessages(this.queueScope, roomJid);
      for (const entry of entries) {
        if (!this.roomIsReady(roomJid) || !this.xmpp) break;
        if (this.inflightQueuedIds.has(entry.id)) continue;
        this.queuedMessageStatusHandler?.(entry.id, "sending");
        const messageId = await this.compatSendGroupMessage(this.xmpp, roomJid, entry.body, { ...(entry.markup?.length ? { markup: entry.markup } : {}), ...(entry.references?.length ? { references: entry.references } : {}), mentionJidsByNick: { ...(entry.mentionJidsByNick ?? {}), ...this.memberJidsFor(roomJid) }, ...(entry.files?.length ? { files: entry.files } : {}), ...(entry.replyTo ? { replyTo: entry.replyTo } : {}), ...(entry.threadId ? { threadId: entry.threadId } : {}), ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}), ...(entry.threadCreate ? { threadCreate: entry.threadCreate } : {}), ...(entry.threadReply ? { threadReply: entry.threadReply } : {}), id: entry.id });
        if (messageId) { this.inflightQueuedIds.add(entry.id); this.recordPendingSend(entry.id, "room"); }
      }
    })();
    this.roomQueueFlushes.set(roomJid, promise.finally(() => { if (this.roomQueueFlushes.get(roomJid) === promise) this.roomQueueFlushes.delete(roomJid); }));
    return this.roomQueueFlushes.get(roomJid);
  }

  async sendGroupMessage(spaceId: string, channelId: string, body: string, opts: SendGroupMessageOptions = {}): Promise<OutboundSendResult | null> {
    const hasFiles = !!opts.files?.length;
    const hasThreadMetadata = !!opts.threadId?.trim();
    const hasForumMetadata = !!opts.threadCreate?.title?.trim() || !!opts.threadReply?.threadId?.trim();
    if (!body.trim() && !hasFiles && !hasThreadMetadata && !hasForumMetadata) return null;
    const roomJid = this.roomJidForChannel(channelId);
    if (this.roomIsReady(roomJid) && this.xmpp) {
      const id = await this.compatSendGroupMessage(this.xmpp, roomJid, body, { ...opts, mentionJidsByNick: { ...(opts.mentionJidsByNick ?? {}), ...this.memberJidsFor(roomJid) } });
      this.recordPendingSend(id, "room");
      return { id, state: "sending" };
    }
    const queued = this.queueRoomMessage(roomJid, body, opts);
    void this.connect().then(() => this.switchRoom(spaceId, channelId)).then(() => this.flushQueuedRoomMessages(roomJid)).catch(() => undefined);
    return queued;
  }

  async sendDirectMessage(peerJid: string, body: string, opts: SendDirectMessageOptions = {}): Promise<OutboundSendResult | null> {
    if (!body.trim() && !opts.files?.length) return null;
    const normalizedPeerJid = barePeerJid(peerJid);
    if (this.canUseConnectedSession() && this.xmpp) {
      const id = await this.compatSendDirectMessage(this.xmpp, normalizedPeerJid, body, opts);
      this.recordPendingSend(id, "dm");
      return { id, state: "sending" };
    }
    return this.queueDirectMessage(normalizedPeerJid, body, opts);
  }

  async sendChatState(spaceId: string, channelId: string, state: ChatStateType, thread?: { id: string; parent?: string }) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendChatState(xmpp, roomJid, "groupchat", state, thread); }
  async sendDisplayed(spaceId: string, channelId: string, messageId: string, thread?: { id: string; parent?: string }) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendDisplayed(xmpp, roomJid, "groupchat", messageId, thread); }
  async sendReaction(spaceId: string, channelId: string, messageId: string, emojis: string[], thread?: { id: string; parent?: string }) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendReaction(xmpp, roomJid, "groupchat", messageId, emojis, thread); }
  /** #414: pin a message in the room. Server gates on Owner/Admin
   * affiliation; non-admins receive a `<forbidden/>` error which
   * surfaces as a rejected Promise. */
  async pinMessage(spaceId: string, channelId: string, targetStanzaId: string) {
    const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId);
    if (typeof xmpp.pin_message !== "function") throw new Error("pin_message not available in wasm client");
    await xmpp.pin_message(roomJid, targetStanzaId);
  }
  /** #414: unpin a message in the room. Same authorization rules. */
  async unpinMessage(spaceId: string, channelId: string, targetStanzaId: string) {
    const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId);
    if (typeof xmpp.unpin_message !== "function") throw new Error("unpin_message not available in wasm client");
    await xmpp.unpin_message(roomJid, targetStanzaId);
  }
  /** #414: fetch the room's current pin list. Server requires the
   * caller to be a current room occupant. Returns entries in
   * pin-time-desc order. */
  async fetchRoomPins(spaceId: string, channelId: string): Promise<import("./wasm-types").WasmPinEntry[]> {
    const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId);
    if (typeof xmpp.fetch_room_pins !== "function") throw new Error("fetch_room_pins not available in wasm client");
    const result = await xmpp.fetch_room_pins(roomJid);
    return Array.isArray(result) ? (result as import("./wasm-types").WasmPinEntry[]) : [];
  }
  /** XEP-0359: fetch a batch of MAM messages by their stanza-ids. */
  async fetchRoomMessagesByStanzaIds(
    spaceId: string,
    channelId: string,
    stanzaIds: string[],
  ): Promise<import("./wasm-types").WasmArchivedMessage[]> {
    if (stanzaIds.length === 0) return [];
    const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId);
    if (typeof xmpp.fetch_room_messages_by_stanza_ids !== "function") throw new Error("fetch_room_messages_by_stanza_ids not available in wasm client");
    const page = (await xmpp.fetch_room_messages_by_stanza_ids(roomJid, stanzaIds)) as import("./wasm-types").WasmMamPage | null;
    return Array.isArray(page?.messages) ? page.messages : [];
  }
  async sendRetraction(spaceId: string, channelId: string, retractsId: string, thread?: { id: string; parent?: string }) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendRetraction(xmpp, roomJid, "groupchat", retractsId, thread); }
  async sendModeration(spaceId: string, channelId: string, targetId: string, reason?: string) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendModeration(xmpp, roomJid, targetId, reason); }
  // Corrections (XEP-0308) flow through compatSendCorrection's options dict,
  // which already serializes thread/parent into the wasm send options used by
  // build_outbound_message — so passing { threadId, parentThreadId } here is
  // all the wire-level conformance needs.
  async sendCorrection(spaceId: string, channelId: string, body: string, replacesId: string, markup?: SendGroupMessageOptions["markup"], references?: SendGroupMessageOptions["references"], thread?: { id: string; parent?: string }): Promise<string | null> {
    const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId);
    const opts: SendGroupMessageOptions = { markup, references };
    if (thread?.id) opts.threadId = thread.id;
    if (thread?.parent) opts.parentThreadId = thread.parent;
    return await this.compatSendCorrection(xmpp, roomJid, "groupchat", body, replacesId, opts);
  }
  async sendDmChatState(peerJid: string, state: ChatStateType): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendChatState(xmpp, barePeerJid(peerJid), "chat", state); }
  async sendDmDisplayed(peerJid: string, messageId: string): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendDisplayed(xmpp, barePeerJid(peerJid), "chat", messageId); }
  async sendDmRetraction(peerJid: string, messageId: string): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendRetraction(xmpp, barePeerJid(peerJid), "chat", messageId); }
  async sendDmCorrection(peerJid: string, body: string, replacesId: string, markup?: SendDirectMessageOptions["markup"], references?: SendDirectMessageOptions["references"]): Promise<string | null> { const xmpp = await this.requireConnectedXmpp(); return await this.compatSendCorrection(xmpp, barePeerJid(peerJid), "chat", body, replacesId, { markup, references }); }
  async sendDmReaction(peerJid: string, messageId: string, emojis: string[]): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendReaction(xmpp, barePeerJid(peerJid), "chat", messageId, emojis); }

  /**
   * XEP-0490 §3 multi-device "read up to here" publish. `chatId` is
   * the bare JID of the chat (DM contact or MUC room); `stanzaId` is
   * the XEP-0359 id of the latest displayed message; `stanzaIdBy` is
   * the JID that injected that stanza-id (room for MUC, user's own
   * server for 1:1). Failures are intentionally silent — MDS is a
   * best-effort multi-device-sync signal, not a UX-visible action.
   */
  async publishMdsDisplayed(chatId: string, stanzaId: string, stanzaIdBy: string): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    if (typeof xmpp.publish_mds_displayed !== "function") return;
    try { await xmpp.publish_mds_displayed(chatId, stanzaId, stanzaIdBy); } catch { /* best-effort */ }
  }

  /**
   * XEP-0490 §3.1 catch-up: fetch every item from the user's own
   * `urn:xmpp:mds:displayed:0` PEP node. Used on bind / initial
   * presence to seed local displayed state for chats the user
   * advanced on another device while this one was offline. Returns
   * an empty array on first call (no node yet) rather than throwing.
   */
  async fetchMdsDisplayed(): Promise<MdsDisplayedEntry[]> {
    const xmpp = await this.requireConnectedXmpp();
    if (typeof xmpp.fetch_mds_displayed !== "function") return [];
    try {
      const raw = await xmpp.fetch_mds_displayed() as ReadonlyArray<WasmMdsDisplayedEntry> | null;
      if (!raw) return [];
      return raw.map((entry) => ({ chatId: entry.chat_id, stanzaId: entry.stanza_id, stanzaIdBy: entry.stanza_id_by }));
    } catch {
      return [];
    }
  }

  /**
   * XEP-0060 explicit subscribe to the MDS node. Used as a fallback
   * path for receiving `+notify` events when the chat client's
   * presence does not yet advertise XEP-0115 caps. Failures are
   * swallowed (the subscribe is best-effort; catch-up still works).
   */
  async subscribeMdsDisplayed(): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    if (typeof xmpp.subscribe_mds_displayed !== "function") return;
    try { await xmpp.subscribe_mds_displayed(); } catch { /* best-effort */ }
  }

  setMdsDisplayedHandler(handler: ((entry: MdsDisplayedEntry) => void) | null) { this.mdsDisplayedHandler = handler; }

  private async resolveUploadService(): Promise<string> {
    if (this.uploadServiceJid) return this.uploadServiceJid;
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.discover_upload_service) throw new Error("XMPP not connected");
    const jid = await discoverUploadService(xmpp as WasmClient);
    if (!jid) throw new Error(`File upload service not available (domain: ${jidDomain(this.session.jid)})`);
    this.uploadServiceJid = jid;
    return jid;
  }

  /**
   * Upload a single file/blob for a story (or other plaintext-broadcast
   * surface). Unlike `uploadAttachments`, the payload is uploaded as-is
   * without OMEMO encryption — stories are community-broadcast content,
   * not DMs.
   *
   * The caller is expected to pre-validate `blob.size <=
   * MAX_FILE_UPLOAD_BYTES`; the upload service may still reject with
   * 413 if its own cap is lower. Filename is replaced with a random
   * UUID-based name so user-facing names don't leak into the public
   * GET URL or OTel spans.
   */
  async uploadStoryMedia(
    blob: Blob | File,
    onProgress?: (progress: UploadProgress) => void,
  ): Promise<{ url: string; size: number; contentType: string }> {
    const xmpp = await this.requireConnectedXmpp();
    const uploadDomain = await this.resolveUploadService();
    const contentType = blob.type || "application/octet-stream";
    const ext = contentType.split("/")[1]?.split(";")[0] || "bin";
    const safeName = `story-${crypto.randomUUID()}.${ext}`;
    const sanitised =
      blob instanceof File
        ? new File([blob], safeName, { type: contentType })
        : new File([blob], safeName, { type: contentType });
    const result = await uploadFile(
      xmpp as WasmClient,
      sanitised,
      uploadDomain,
      onProgress,
    );
    return { url: result.getUrl, size: result.size, contentType: result.contentType };
  }

  async uploadAttachments(files: ReadonlyArray<File | Blob>, onProgress?: (overall: UploadProgress, index: number) => void): Promise<OutboundFileAttachment[]> {
    const xmpp = await this.requireConnectedXmpp();
    const uploadDomain = await this.resolveUploadService();
    const preparedUploads = await Promise.all(files.map((file) => prepareEncryptedAttachmentUpload(file)));
    const totals = preparedUploads.map((prepared) => prepared.uploadFile.size);
    const loaded = files.map(() => 0);
    const grandTotal = totals.reduce((a, b) => a + b, 0);
    const results: OutboundFileAttachment[] = [];
    for (const [index, prepared] of preparedUploads.entries()) {
      const result = await uploadFile(xmpp as WasmClient, prepared.uploadFile, uploadDomain, (progress) => {
        loaded[index] = progress.loaded;
        onProgress?.({ loaded: loaded.reduce((a, b) => a + b, 0), total: grandTotal }, index);
      });
      results.push({ url: result.getUrl, name: prepared.originalName, mediaType: prepared.originalMediaType, size: prepared.originalSize, disposition: inferredFileDisposition(prepared.originalMediaType, prepared.originalName), encrypted: { ...prepared.encrypted, sources: [result.getUrl] } });
    }
    return results;
  }

  async invokeExtensionLaunch(launch: any): Promise<ExtensionCommandResult> { const xmpp = await this.requireConnectedXmpp(); return invokeExtensionLaunch(xmpp as WasmClient, this.session.jid, launch); }
  async discoverExtensionCommands(): Promise<DiscoveredExtensionCommand[]> { const xmpp = await this.requireConnectedXmpp(); return discoverExtensionCommands(xmpp as WasmClient, this.session.jid); }
  async discoverExtensionRoutes(): Promise<DiscoveredExtensionRoute[]> { const xmpp = await this.requireConnectedXmpp(); return discoverExtensionRoutes(xmpp as WasmClient, this.session.jid); }
  async fetchExtensionRouteItems(route: DiscoveredExtensionRoute, roomJid: string): Promise<ExtensionRouteItem[]> { const xmpp = await this.requireConnectedXmpp(); return fetchExtensionRouteItems(xmpp as WasmClient, route, roomJid); }
  async invokeExtensionCommand(command: DiscoveredExtensionCommand): Promise<ExtensionCommandResult> { const xmpp = await this.requireConnectedXmpp(); return invokeExtensionCommand(xmpp as WasmClient, this.session.jid, command); }
  async submitExtensionCommandForm(command: DiscoveredExtensionCommand, sessionId: string, fields: ExtensionCommandFormField[], action?: ExtensionCommandAction, roomJid?: string): Promise<ExtensionCommandResult> { const xmpp = await this.requireConnectedXmpp(); return submitExtensionCommandForm(xmpp as WasmClient, command, sessionId, fields, action, roomJid); }

  async enablePushNotifications(opts: { serviceJid: string; node: string }): Promise<boolean> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.enable_push_notifications) return false;
    try {
      await xmpp.enable_push_notifications(opts.serviceJid, opts.node);
      return true;
    } catch (error) {
      console.warn("[xmpp] XEP-0357 enable IQ rejected:", error);
      return false;
    }
  }

  /**
   * XEP-0357 §6.1 `<disable/>` IQ. Passing `node: undefined` disables
   * ALL push nodes at the service for this user (the "disable push
   * everywhere" account-settings flow). Passing a specific node id
   * disables only that one — leaves other registrations alone.
   */
  async disablePushNotifications(opts: { serviceJid: string; node?: string }): Promise<boolean> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.disable_push_notifications) return false;
    try {
      await xmpp.disable_push_notifications(opts.serviceJid, opts.node);
      return true;
    } catch (error) {
      console.warn("[xmpp] XEP-0357 disable IQ rejected:", error);
      return false;
    }
  }

  /**
   * Fetch the Push Service's currently-active VAPID public key + kid
   * via the XEP-0128 disco extension form
   * (`FORM_TYPE='urn:waddle:push:vapid:0'`).
   *
   * Resolves to `null` in any of these cases — callers can't distinguish
   * them from the return value alone, but each path emits a precise
   * `console.warn` for diagnostics:
   *   - The server doesn't advertise the form (Web Push not configured)
   *   - The session is not ready (mid-reconnect / mid-teardown race)
   *   - The disco IQ times out after 10s
   *   - The wasm `fetch_vapid_public_key` method is missing (older bundle)
   *   - The IQ is rejected (transport error, stanza error, malformed
   *     advertisement caught by the wasm-side wire-shape validator)
   *   - The JS-boundary shape is unexpected (regression on the Rust side)
   *
   * This is a `null`-on-any-error contract by design: every failure
   * mode collapses to the same caller-visible action (fall back to the
   * foreground Notification API; retry on next reconnect). UI surfaces
   * that need to distinguish "not configured" from "transient error"
   * should consume the console warnings via a log sink, not branch on
   * the return value.
   */
  async fetchVapidPublicKey(opts: { serviceJid: string }): Promise<{ publicKey: string; kid: string } | null> {
    // Bounded timeout (10s): the wasm-side `send_iq_command` is itself
    // an unbounded oneshot. Without this race the rotation lock can be
    // held indefinitely on a stalled Push Service handler, blocking
    // every subsequent enable + reconnect's setupPushSubscription.
    const TIMEOUT_MS = 10_000;
    let timeoutHandle: ReturnType<typeof setTimeout> | null = null;
    let timedOut = false;
    const timeoutSentinel = Symbol("vapid-fetch-timeout");
    const timeoutPromise = new Promise<typeof timeoutSentinel>((resolve) => {
      timeoutHandle = setTimeout(() => {
        timedOut = true;
        resolve(timeoutSentinel);
      }, TIMEOUT_MS);
    });
    try {
      // `requireConnectedXmpp` throws when the session is not ready
      // (mid-reconnect / mid-teardown race). The JSDoc contract for
      // this method is "returns null on any error so the caller can
      // fall back to foreground Notifications" — so the readiness
      // check goes INSIDE the try block. Without this, a brief
      // reconnect race during setupPushSubscription would abort the
      // whole rotation flow instead of degrading cleanly.
      const xmpp = await this.requireConnectedXmpp();
      if (!xmpp.fetch_vapid_public_key) {
        // Bundle-drift diagnostic: the JS shipped expects a wasm
        // method that the active bundle doesn't export. Typically
        // means the chat upgraded its TS but the @waddle/xmpp-client-wasm
        // package is older. Surface so operators see why Web Push
        // silently fell back to the foreground Notification API.
        console.warn(
          "[xmpp] fetch_vapid_public_key is unavailable (older wasm bundle?); returning null",
        );
        return null;
      }
      const result = await Promise.race([
        xmpp.fetch_vapid_public_key(opts.serviceJid),
        timeoutPromise,
      ]);
      if (result === timeoutSentinel) {
        console.warn(
          `[xmpp] fetch_vapid_public_key timed out after ${TIMEOUT_MS}ms; ` +
          "Push Service may be unreachable. Returning null so the chat falls back " +
          "to the foreground Notification API; the next reconnect will retry.",
        );
        return null;
      }
      if (result === null || result === undefined) return null;
      const candidate = result as { publicKey?: unknown; kid?: unknown };
      // Validate the JS-boundary shape — the wasm method's signature
      // is `Promise<any>` after wasm-bindgen, so a regression on the
      // Rust side that emitted an unexpected object would otherwise
      // silently propagate. Hard-fail on shape drift.
      if (
        typeof candidate.publicKey !== "string" ||
        candidate.publicKey.length === 0 ||
        typeof candidate.kid !== "string" ||
        candidate.kid.length === 0
      ) {
        console.warn(
          "[xmpp] fetch_vapid_public_key returned an unexpected shape:",
          candidate,
        );
        return null;
      }
      return { publicKey: candidate.publicKey, kid: candidate.kid };
    } catch (error) {
      if (timedOut) {
        // The wasm-side IQ rejected after we already gave up on it —
        // the warning above already explained the timeout; this path
        // just drops the late result silently.
        return null;
      }
      console.warn("[xmpp] fetch_vapid_public_key IQ rejected:", error);
      return null;
    } finally {
      if (timeoutHandle !== null) clearTimeout(timeoutHandle);
    }
  }

  /**
   * Register a Web Push device with the XMPP Push Service via the
   * XEP-0050 `register-device` ad-hoc command. The composer drives
   * the multi-step §3 dance (execute → executing+form → complete →
   * completed+result) inside the WASM bridge and returns the
   * assigned XEP-0357 node id — no separate `ensure-node` round
   * trip is needed.
   *
   * The three Web Push fields come from a browser `PushSubscription`:
   *
   *   * `endpoint`  ← `subscription.endpoint`
   *   * `p256dh`    ← `subscription.toJSON().keys.p256dh`
   *   * `auth`      ← `subscription.toJSON().keys.auth`
   *
   * **Not** idempotent: every call mints a fresh server-assigned
   * `deviceId` and persists a new `push_devices` row. The chat's
   * `syncPushSubscriptionImpl` is responsible for short-circuiting
   * a re-enable when localStorage already carries a `(node,
   * deviceId)` pair (tracked separately under follow-up #768).
   *
   * `appId="web"` is the convention for the browser/PWA chat. APNs
   * and FCM follow with `"ios"` / `"android"` in later PRs.
   */
  async registerPushDevice(opts: {
    serviceJid: string;
    appId: string;
    environment: "prod" | "sandbox";
    endpoint: string;
    p256dh: string;
    auth: string;
  }): Promise<RegisterDeviceResult | null> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.register_push_device) return null;
    try {
      const result = await xmpp.register_push_device({
        serviceJid: opts.serviceJid,
        appId: opts.appId,
        environment: opts.environment,
        platform: "web",
        endpoint: opts.endpoint,
        p256dh: opts.p256dh,
        auth: opts.auth,
      });
      // The chat MUST persist BOTH `node` and `deviceId`; a missing
      // `deviceId` would force the disable-device opt-out into
      // disable-everywhere semantics that take down sibling devices
      // on the same XEP-0357 node. `parseRegisterDeviceResult` enforces
      // that invariant (unit-tested in `push-register-result.test.ts`).
      const parsed = parseRegisterDeviceResult(result);
      if (!parsed) {
        console.warn(
          "[xmpp] register_push_device returned an empty node/deviceId; refusing to persist",
        );
        return null;
      }
      return parsed;
    } catch (error) {
      console.warn("[xmpp] XEP-0050 register-device rejected:", error);
      return null;
    }
  }

  /**
   * XEP-0050 `disable-device` ad-hoc command on the Push Service.
   * Removes ONLY this device's row from the stable per-(user, app-id)
   * node, leaving other devices on the same node alone.
   *
   * The XEP-0357 `<disable jid='…' node='…'/>` on the user-server
   * (see `disablePushNotifications`) is NODE-LEVEL: it removes the
   * entire `(push-service-jid, node)` pair from the user-server's
   * registration list, which silently stops fan-out to every
   * device on the node. The chat MUST NOT call both from the
   * per-device opt-out path — that would take down push for
   * other installations.
   *
   * The "disable push everywhere" flow (a dedicated UI affordance
   * with explicit user warning) is the only place that should call
   * both APIs.
   */
  async disablePushDevice(opts: { serviceJid: string; node: string; deviceId: string }): Promise<boolean> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.disable_push_device) return false;
    try {
      await xmpp.disable_push_device(opts.serviceJid, opts.node, opts.deviceId);
      return true;
    } catch (error) {
      console.warn("[xmpp] XEP-0050 disable-device rejected:", error);
      return false;
    }
  }

  /**
   * Fetch the user's XEP-0402 bookmarks from PEP, surfacing each one
   * with its XEP-0492 fallback notification mode when present (#532).
   *
   * Returns an empty list when the user has not yet published any
   * bookmarks — that is the on-first-connect state and the chat UI
   * resolves per-conversation defaults via [[resolveDefaultNotifyMode]]
   * per XEP-0492 §3.
   */
  async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
    // Wrap the whole call — including `requireConnectedXmpp()` — so
    // a reconnect/teardown race (the client instance exists but the
    // session isn't ready) doesn't escape as an unhandled
    // rejection. `notifySettingsStore.hydrate` is dispatched with
    // `void` from the lifecycle handler, so a thrown rejection here
    // becomes an unhandled Promise rejection on flaky networks
    // exactly when hydrate fires. Round-14 PR review.
    try {
      const xmpp = await this.requireConnectedXmpp();
      if (!xmpp.fetch_user_bookmarks) return [];
      const items = (await xmpp.fetch_user_bookmarks()) as UserBookmarkItem[] | null;
      return items ?? [];
    } catch (error) {
      console.warn("[xmpp] XEP-0402 bookmark fetch failed:", error);
      return [];
    }
  }

  /**
   * Set the XEP-0492 fallback notification mode for one room.
   *
   * The WASM bridge is fetch-merge-publish: it preserves the rest of
   * the user's bookmark for that room (name, autojoin) as well as
   * foreign extensions / identity-scoped notify siblings that another
   * client may have written (XEP-0492 §3 first paragraph). On success,
   * resolves to the updated bookmark — the chat store should replace
   * its cached entry with this value rather than trusting the
   * requested mode in isolation.
   */
  async setRoomNotificationMode(opts: {
    roomJid: string;
    mode: "always" | "on-mention" | "never";
    name?: string;
  }): Promise<SetRoomNotificationModeOutcome> {
    // Wrap the WHOLE call — including `requireConnectedXmpp()` — so
    // a reconnect/teardown race (the client instance exists but the
    // session isn't ready) doesn't escape as an exception and break
    // the typed-outcome contract documented at the call site.
    // Round-13 PR review.
    try {
      const xmpp = await this.requireConnectedXmpp();
      if (!xmpp.set_room_notification_mode) return { kind: "error" };
      // WASM resolves with a typed tagged outcome — no stringly-typed
      // condition transport across the JS↔Rust boundary (round-13 PR
      // compliance #19). The specific RFC 6120 §8.3 condition stays
      // on the Rust side as a tracing diagnostic; chat-side
      // branching is on the `kind` discriminator only.
      const outcome = (await xmpp.set_room_notification_mode({
        roomJid: opts.roomJid,
        mode: opts.mode,
        name: opts.name,
      })) as SetRoomNotificationModeOutcome;
      if (outcome.kind === "error") {
        console.warn("[xmpp] XEP-0492 bookmark publish rejected");
      }
      return outcome;
    } catch (error) {
      // Reached for session-not-ready exceptions thrown from
      // requireConnectedXmpp, plus transport / serialization
      // failures. Stanza errors are surfaced via the typed outcome
      // above and never reach here.
      console.warn("[xmpp] XEP-0492 bookmark publish failed:", error);
      return { kind: "error" };
    }
  }

  /**
   * Fetch the user's XEP-0430 inbox. The wasm bridge runs the
   * streaming reducer (sends `<inbox/>` IQ, accumulates the streamed
   * `<message><entry/></message>` frames matching the IQ's `queryid`,
   * resolves once the closing `<fin/>` IQ arrives).
   */
  async fetchInbox(opts: FetchInboxOptions = {}): Promise<InboxResult> {
    const xmpp = await this.requireConnectedXmpp();
    const result = await xmpp.fetch_inbox?.({
      ...(opts.onlyUnread ? { only_unread: true } : {}),
      ...(opts.noMessages ? { no_messages: true } : {}),
    }) as WasmInboxResult | undefined;
    return {
      totalUnread: result?.total_unread ?? 0,
      total: result?.total ?? (result?.conversations?.length ?? 0),
      unreadConversations:
        result?.unread_conversations
          ?? (result?.conversations ?? []).filter((c) => c.unread > 0).length,
      conversations: (result?.conversations ?? []).map(inboxEntryFromWasm),
    };
  }

  async markInboxRead(partnerJid: string, threadId?: string): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.mark_inbox_read?.(barePeerJid(partnerJid), threadId ?? null); }

  /**
   * Fetch the global cross-channel threads view (`urn:waddle:threads:0`).
   * Returns an empty page when the wasm client lacks the method or the
   * server doesn't respond — callers can render the empty state.
   */
  async fetchThreads(opts: { pageSize?: number; afterCursor?: string } = {}): Promise<WasmThreadsPage> {
    const xmpp = await this.requireConnectedXmpp();
    const payload: WasmFetchThreadsOptions = {
      ...(typeof opts.pageSize === "number" ? { page_size: opts.pageSize } : {}),
      ...(opts.afterCursor ? { after_cursor: opts.afterCursor } : {}),
    };
    const raw = await xmpp.fetch_threads?.(payload) as WasmThreadsPage | undefined;
    return raw ?? { total: 0, unread_threads: 0, entries: [] };
  }

  /**
   * Fetch the latest XEP-0472 social feed entries from the community
   * service. Returns entries newest-first as the server orders them.
   */
  async fetchFeed(communityJid: string, maxItems: number | undefined = undefined): Promise<FeedEntry[]> {
    const xmpp = await this.requireConnectedXmpp();
    const result = await xmpp.feed_items?.(communityJid, maxItems ?? null) as WasmFeedEntry[] | undefined;
    return (result ?? []).map(feedEntryFromWasm);
  }

  /**
   * Publish a XEP-0472 entry to the community feed. Resolves with the
   * server-confirmed entry (with id + published) so callers can append
   * to local state without re-fetching.
   */
  async publishFeedPost(communityJid: string, post: FeedPostInput): Promise<FeedEntry> {
    const xmpp = await this.requireConnectedXmpp();
    const result = await xmpp.feed_publish?.(communityJid, {
      body: post.body,
      ...(post.title ? { title: post.title } : {}),
      ...(post.author ? { author: post.author } : {}),
      ...(post.link ? { link: post.link } : {}),
    }) as WasmFeedEntry | undefined;
    if (!result) {
      throw new Error("feed_publish returned no entry");
    }
    return feedEntryFromWasm(result);
  }

  /**
   * Fetch the latest XEP-0501 stories from the community stories
   * node. Returns ALL items (including expired); the chat filters
   * active vs expired locally.
   */
  async fetchStories(communityJid: string, maxItems: number | undefined = undefined): Promise<Story[]> {
    const xmpp = await this.requireConnectedXmpp();
    const result = await xmpp.stories_items?.(communityJid, maxItems ?? null) as WasmStory[] | undefined;
    return (result ?? []).map(storyFromWasm);
  }

  /**
   * Fetch the current user's story read-state from their private PEP
   * node (XEP-0223 `urn:waddle:story:reads:0`). Read-state is
   * non-critical: any failure (no item yet, network error, spoofed
   * `from`) silently produces an empty result rather than surfacing
   * an error to the UI.
   */
  async fetchStoryReads(): Promise<StoryReads> {
    const xmpp = await this.requireConnectedXmpp();
    const result = (await xmpp.story_reads_fetch?.()) as WasmStoryReads | undefined;
    return storyReadsFromWasm(result);
  }

  /**
   * Publish the current user's story read-state. Overwrites the
   * single `current` item on every call.
   */
  async publishStoryReads(reads: StoryReads): Promise<StoryReads> {
    const xmpp = await this.requireConnectedXmpp();
    const result = (await xmpp.story_reads_publish?.(
      storyReadsToWasm(reads),
    )) as WasmStoryReads | undefined;
    return storyReadsFromWasm(result);
  }

  /**
   * Publish a XEP-0501 story. At least one of `body`/`mediaUrl` is
   * required (the server rejects empty stories).
   */
  async publishStory(communityJid: string, input: StoryPostInput): Promise<Story> {
    const xmpp = await this.requireConnectedXmpp();
    const result = await xmpp.stories_publish?.(communityJid, {
      ...(input.body ? { body: input.body } : {}),
      ...(input.mediaUrl ? { media_url: input.mediaUrl } : {}),
      ...(input.author ? { author: input.author } : {}),
      ...(typeof input.expiryHours === "number" ? { expiry_hours: input.expiryHours } : {}),
    }) as WasmStory | undefined;
    if (!result) {
      throw new Error("stories_publish returned no story");
    }
    return storyFromWasm(result);
  }

  /**
   * Fetch xCal community events. Returns ALL items (past + upcoming);
   * the chat sorts upcoming-first.
   */
  async fetchCommunityEvents(communityJid: string, maxItems: number | undefined = undefined): Promise<CommunityEvent[]> {
    const xmpp = await this.requireConnectedXmpp();
    const result = await xmpp.xcal_items?.(communityJid, maxItems ?? null) as WasmVEvent[] | undefined;
    return (result ?? []).map(eventFromWasm);
  }

  /**
   * Publish an xCal community event. SUMMARY is required; DTSTART
   * is required for the event to be useful on a timeline.
   */
  async publishCommunityEvent(communityJid: string, input: CommunityEventInput): Promise<CommunityEvent> {
    const xmpp = await this.requireConnectedXmpp();
    const result = await xmpp.xcal_publish?.(communityJid, {
      summary: input.summary,
      ...(input.description ? { description: input.description } : {}),
      ...(input.location ? { location: input.location } : {}),
      ...(input.organizer ? { organizer: input.organizer } : {}),
      ...(typeof input.dtstartMs === "number"
        ? { dtstart: new Date(input.dtstartMs).toISOString() }
        : {}),
      ...(typeof input.dtendMs === "number"
        ? { dtend: new Date(input.dtendMs).toISOString() }
        : {}),
      ...(input.rrule ? { rrule: rruleToWasm(input.rrule) } : {}),
    }) as WasmVEvent | undefined;
    if (!result) {
      throw new Error("xcal_publish returned no event");
    }
    return eventFromWasm(result);
  }

  /**
   * Replace an existing community event item with new master values.
   * Uses `xcal_publish_item` which atomically overwrites the item at
   * `itemId`, preserving any sibling RSVPs (those live on their own
   * `-rsvp-*` item ids).
   */
  async updateCommunityEvent(
    communityJid: string,
    itemId: string,
    input: CommunityEventInput,
  ): Promise<CommunityEvent> {
    const xmpp = await this.requireConnectedXmpp();
    const exdates = (input.exdatesMs ?? []).map((ms) => new Date(ms).toISOString());
    const result = await xmpp.xcal_publish_item?.(communityJid, itemId, {
      master: {
        summary: input.summary,
        ...(input.description ? { description: input.description } : {}),
        ...(input.location ? { location: input.location } : {}),
        ...(input.organizer ? { organizer: input.organizer } : {}),
        ...(typeof input.dtstartMs === "number"
          ? { dtstart: new Date(input.dtstartMs).toISOString() }
          : {}),
        ...(typeof input.dtendMs === "number"
          ? { dtend: new Date(input.dtendMs).toISOString() }
          : {}),
        ...(input.rrule ? { rrule: rruleToWasm(input.rrule) } : {}),
      },
      overrides: [],
      exdates,
    }) as WasmVEvent | undefined;
    if (!result) {
      throw new Error("xcal_publish_item returned no event");
    }
    return eventFromWasm(result);
  }

  /**
   * Retract (delete) a community event item. Removes the master plus
   * any per-instance overrides in one shot; sibling RSVP items live
   * under their own ids and stay behind until separately retracted.
   */
  async retractCommunityEvent(communityJid: string, itemId: string): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    await xmpp.xcal_retract?.(communityJid, itemId);
  }

  /**
   * Publish (or update) this session's RSVP for a calendar event.
   * The server bridges the call to a sibling pubsub item; the chat
   * folds it back into the master event on the next refresh.
   */
  async rsvpCommunityEvent(
    communityJid: string,
    masterUid: string,
    selfLocalpart: string,
    selfBareJid: string,
    partstat: PartStat,
  ): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    await xmpp.xcal_rsvp?.(communityJid, masterUid, selfLocalpart, selfBareJid, partstat);
  }
  async publishMood(mood: MoodPublication): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.publish_mood?.({ kind: mood.kind, ...(mood.text ? { text: mood.text } : {}) }); }
  async retractMood(): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.retract_mood?.(); }
  async publishActivity(activity: ActivityPublication): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.publish_activity?.({ general: activity.general, ...(activity.specific ? { specific: activity.specific } : {}), ...(activity.text ? { text: activity.text } : {}) }); }
  async retractActivity(): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.retract_activity?.(); }
  async publishTune(tune: TunePublication): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.publish_tune?.(tune); }
  async retractTune(): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.retract_tune?.(); }
  async fetchUserPepProfile(jid: string): Promise<UserPepProfile> {
    const xmpp = await this.requireConnectedXmpp();
    const profile = await xmpp.fetch_user_pep_profile?.(jid) as WasmPepProfile | null;
    return { mood: profile?.mood ? { kind: profile.mood.kind as any, ...(profile.mood.text ? { text: profile.mood.text } : {}) } : null, activity: profile?.activity ? { general: profile.activity.general as any, ...(profile.activity.specific ? { specific: profile.activity.specific } : {}), ...(profile.activity.text ? { text: profile.activity.text } : {}) } : null, tune: profile?.tune ? { ...profile.tune } : null };
  }
  async fetchVCard4(jid: string): Promise<VCard4Profile | null> {
    const xmpp = await this.requireConnectedXmpp();
    const payload = await xmpp.fetch_vcard4?.(jid) as WasmVCard4 | null;
    if (!payload) return null;
    const profile: VCard4Profile = {};
    if (payload.fn) profile.fullName = payload.fn;
    if (payload.nickname) profile.nickname = payload.nickname;
    if (payload.pronouns) profile.pronouns = payload.pronouns;
    if (payload.note) profile.note = payload.note;
    if (payload.url) profile.url = payload.url;
    return profile;
  }
  async publishVCard4(profile: VCard4Profile): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    const payload: WasmVCard4 = {};
    if (profile.fullName) payload.fn = profile.fullName;
    if (profile.nickname) payload.nickname = profile.nickname;
    if (profile.pronouns) payload.pronouns = profile.pronouns;
    if (profile.note) payload.note = profile.note;
    if (profile.url) payload.url = profile.url;
    await xmpp.publish_vcard4?.(payload);
  }

  private roomMamPageToMessages(page: WasmMamPage): MamHistoryPage<LiveRoomMessage> {
    return { messages: page.messages.map((message) => roomMessageFromArchived(message)).filter((message): message is LiveRoomMessage => !!message), ...(page.first_id ? { firstArchiveId: page.first_id } : {}), ...(page.last_id ? { lastArchiveId: page.last_id } : {}), complete: page.is_complete };
  }
  private dmMamPageToMessages(
    page: WasmMamPage,
    options: { applyCallEvents?: boolean } = {},
  ): MamHistoryPage<LiveDmMessage> {
    const selfBare = barePeerJid(this.session.jid);
    if (options.applyCallEvents !== false) {
      this.applyDmCallEventsFromMamPage(page, selfBare);
    }
    return { messages: page.messages.map((message) => dmMessageFromArchived(message, selfBare)).filter((message): message is LiveDmMessage => !!message), ...(page.first_id ? { firstArchiveId: page.first_id } : {}), ...(page.last_id ? { lastArchiveId: page.last_id } : {}), complete: page.is_complete };
  }
  private applyDmCallEventsFromMamPage(
    page: WasmMamPage | null | undefined,
    selfBare = barePeerJid(this.session.jid),
    options: { since?: string; seenIds?: ReadonlyArray<string> } = {},
  ): void {
    for (const message of page?.messages ?? []) {
      if (!message.call_event) continue;
      if (shouldSkipRawCatchupMessage(message, options.since, options.seenIds)) continue;
      applyDmCallEvent({
        event: message.call_event,
        selfBareJid: selfBare,
        selfFullJid: this.fullJid,
        to: message.to,
        timestamp: message.timestamp,
      });
    }
  }
  private recordRoomMamWatermarks(messages: ReadonlyArray<LiveRoomMessage>) {
    for (const message of messages) {
      this.catchup.recordRoomSeen(message.roomJid, message.createdAt, message.archiveId, messageSeenIds(message));
    }
  }
  private recordDmMamWatermarks(messages: ReadonlyArray<LiveDmMessage>) {
    for (const message of messages) {
      this.catchup.recordDmSeen(message.peerJid, message.createdAt, message.archiveId, messageSeenIds(message));
    }
  }

  async queryMam(spaceId: string, channelId: string, max = 50): Promise<LiveRoomMessage[]> { const page = await this.queryMamPage(spaceId, channelId, max, { type: "latest" }); return page.messages; }
  async queryMamPage(spaceId: string, channelId: string, max = 100, pageParam: MamPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> {
    await this.connect(); await this.switchRoom(spaceId, channelId); const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_room_history_page?.(this.roomJidForChannel(channelId), max, pageParam) as WasmMamPage;
    if (!page) return { messages: [], complete: true };
    const result = this.roomMamPageToMessages(page);
    this.recordRoomMamWatermarks(result.messages);
    return result;
  }
  async queryMamByThread(spaceId: string, channelId: string, threadId: string, max = 100): Promise<LiveRoomMessage[]> {
    await this.connect(); await this.switchRoom(spaceId, channelId); const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_room_history_by_thread?.(this.roomJidForChannel(channelId), threadId, max, null) as WasmMamPage;
    if (!page) return [];
    const result = this.roomMamPageToMessages(page);
    this.recordRoomMamWatermarks(result.messages);
    return result.messages;
  }
  async queryMamThreadPage(spaceId: string, channelId: string, threadId: string, max = 100, pageParam: MamThreadPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> {
    if (!threadId) return { messages: [], complete: true };
    await this.connect(); await this.switchRoom(spaceId, channelId); const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_room_history_by_thread?.(this.roomJidForChannel(channelId), threadId, max, pageParam.type === "before" ? pageParam.before : null) as WasmMamPage;
    if (!page) return { messages: [], complete: true };
    const result = this.roomMamPageToMessages(page);
    this.recordRoomMamWatermarks(result.messages);
    return result;
  }
  async searchMessages(_spaceId: string, channelId: string, query: string, max = 20): Promise<MessageSearchResult[]> {
    if (!query.trim()) return [];
    const xmpp = await this.requireConnectedXmpp();
    const page = await xmpp.search_room_history?.(this.roomJidForChannel(channelId), query, max) as WasmMamPage;
    const parsed = page ? this.roomMamPageToMessages(page).messages : [];
    return parsed.filter((message) => !!message.body).map((message, index) => ({ id: message.id, ...(page?.messages[index]?.mam_id ? { archiveId: page.messages[index].mam_id } : {}), nick: message.nick, body: message.body, createdAt: message.createdAt, ...(message.threadId ? { threadId: message.threadId } : {}), ...(message.parentThreadId ? { parentThreadId: message.parentThreadId } : {}), roomJid: message.roomJid }));
  }
  async queryPersonalMam(peerJid: string, max = 100): Promise<LiveDmMessage[]> { const page = await this.queryPersonalMamPage(peerJid, max, { type: "latest" }); return page.messages; }
  async queryPersonalMamPage(peerJid: string, max = 100, pageParam: MamPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveDmMessage>> {
    const xmpp = await this.requireConnectedXmpp(); const page = await xmpp.fetch_dm_history_page?.(barePeerJid(peerJid), max, pageParam) as WasmMamPage;
    if (!page) return { messages: [], complete: true };
    const result = this.dmMamPageToMessages(page, { applyCallEvents: pageParam.type !== "before" });
    this.recordDmMamWatermarks(result.messages);
    return result;
  }
  async hydrateRecentDmCallActivity(
    peerJid: string,
    options: DmCallActivityHydrationOptions = {},
  ): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.fetch_dm_history_page) return;
    const peer = barePeerJid(peerJid);
    if (!peer) return;
    const sessionJid = this.session.jid;
    const fetchDmHistoryPage = xmpp.fetch_dm_history_page.bind(xmpp);
    const pages = await collectRecentMamPages(
      (max, pageParam) => fetchDmHistoryPage(peer, max, pageParam) as Promise<WasmMamPage>,
      options,
      () => this.isCurrentConnectedXmpp(xmpp, sessionJid),
    );
    if (!this.isCurrentConnectedXmpp(xmpp, sessionJid)) return;
    const selfBare = barePeerJid(sessionJid);
    for (const page of [...pages].reverse()) {
      this.applyDmCallEventsFromMamPage(page, selfBare);
    }
  }
  async hydrateRecentDmCallActivities(
    options: DmCallActivityHydrationOptions = {},
  ): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.fetch_personal_history_page) return;
    const sessionJid = this.session.jid;
    const fetchPersonalHistoryPage = xmpp.fetch_personal_history_page.bind(xmpp);
    const pages = await collectRecentMamPages(
      (max, pageParam) => fetchPersonalHistoryPage(max, pageParam),
      options,
      () => this.isCurrentConnectedXmpp(xmpp, sessionJid),
    );
    if (!this.isCurrentConnectedXmpp(xmpp, sessionJid)) return;
    const selfBare = barePeerJid(sessionJid);
    for (const page of [...pages].reverse()) {
      this.applyDmCallEventsFromMamPage(page, selfBare);
    }
  }
  async searchDmMessages(peerJid: string, query: string, max = 20): Promise<MessageSearchResult[]> {
    if (!query.trim()) return [];
    const xmpp = await this.requireConnectedXmpp();
    const page = await xmpp.search_dm_history?.(barePeerJid(peerJid), query, max) as WasmMamPage;
    const parsed = page ? this.dmMamPageToMessages(page, { applyCallEvents: false }).messages : [];
    return parsed.filter((message) => !!message.body).map((message, index) => ({ id: message.id, ...(page?.messages[index]?.mam_id ? { archiveId: page.messages[index].mam_id } : {}), nick: message.nick, body: message.body, createdAt: message.createdAt, ...(message.threadId ? { threadId: message.threadId } : {}), ...(message.parentThreadId ? { parentThreadId: message.parentThreadId } : {}), peerJid: message.peerJid }));
  }
  async subscribeToPeerPresence(peerJid: string): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.subscribe_to_presence?.(barePeerJid(peerJid)); }
  async listRosterContacts(): Promise<RosterContact[]> { const xmpp = await this.requireConnectedXmpp(); const roster = await xmpp.list_roster_contacts?.() as WasmRosterContact[]; return (roster ?? []).map((item) => { const jid = barePeerJid(item.jid); return { jid, name: item.name, username: item.name?.trim() || jid.split("@")[0] || jid, subscription: (item.subscription ?? "none") as RosterContact["subscription"], groups: item.groups ?? [] }; }); }
  async getServerVersion(): Promise<WasmServerVersion | null> { const xmpp = await this.requireConnectedXmpp(); return await xmpp.get_server_version?.() as WasmServerVersion | null; }
  async discoverSpaceChannels(): Promise<any[]> { const xmpp = await this.requireConnectedXmpp(); return discoverChannels(xmpp as WasmClient, this.session.jid); }
  async discoverTopology(): Promise<DiscoveredTopology> {
    const xmpp = await this.requireConnectedXmpp();
    const topology = await discoverTopology(xmpp as WasmClient, this.session.jid);
    this.discoveredRoomJids = new Map(topology.rooms.flatMap((room) => room.jid ? [[room.id, room.jid] as const] : []));
    const autoJoinRoomJids = topology.rooms
      .filter((room) => room.autojoin !== false && !!room.jid)
      .map((room) => room.jid!);
    this.autoJoinRoomJids = autoJoinRoomJids;
    if (autoJoinRoomJids.length > 0) void this.fanOutAutoJoin(autoJoinRoomJids);
    return topology;
  }
  async listRoomMembers(channelId: string, options?: ListRoomMembersOptions): Promise<MemberSummary[]> {
    const xmpp = await this.requireConnectedXmpp(); const roomJid = options?.roomJid ?? this.roomJidForChannel(channelId);
    const listMembers = xmpp.list_room_members
      ? async (affiliation: "owner" | "admin" | "member" | "outcast") => await xmpp.list_room_members?.(roomJid, affiliation) as WasmRoomMember[]
      : null;
    if (!listMembers) { this.emitError({ kind: "member-query", recoverable: false, detail: "missing list_room_members" }); throw new Error("missing list_room_members"); }
    const affiliations = ["owner", "admin", "member", "outcast"] as const; const members: MemberSummary[] = []; const failedAffiliations: string[] = [];
    for (const affiliation of affiliations) {
      try { const result = await listMembers(affiliation); for (const item of result ?? []) { if (!item.jid) continue; members.push({ jid: item.jid, username: item.jid.split("@")[0] ?? item.jid, avatar_url: null, affiliation, joined_at: "" }); } } catch (error: any) { failedAffiliations.push(affiliation); const condition = error?.condition ?? error?.error?.condition; const detail = condition === "forbidden" ? `forbidden affiliation query — ${roomJid}` : condition === "service-unavailable" ? `unsupported member query — ${roomJid}` : `affiliation query failed for ${affiliation} — ${roomJid}; reconstructed room JID may not match`; this.emitError({ kind: "member-query", recoverable: true, detail, cause: error, condition }); }
    }
    if (members.length === 0 && failedAffiliations.length > 0) {
      throw new RoomMemberListUnavailableError();
    }
    return members;
  }
  async setRoomAffiliation(channelId: string, jid: string, affiliation: MemberSummary["affiliation"]): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await xmpp.set_room_affiliation?.(this.roomJidForChannel(channelId), jid, affiliation === "none" ? "none" : affiliation); }
  /**
   * Probe the `urn:waddle:admin:users:list:0` ad-hoc command to
   * decide whether the authenticated user is the community owner. The
   * underlying wasm binding swallows stanza errors and returns `false`
   * — that includes `<forbidden/>` (not an owner) and transient
   * failures (server-side errors). The admin route treats `false` as
   * "show the empty state" in both cases.
   */
  async isCommunityOwner(): Promise<boolean> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.is_community_owner) return false;
    try { return await xmpp.is_community_owner(); } catch { return false; }
  }
  /**
   * Call the admin Users panel back-end and return one page of users.
   * `prefix` is case-insensitive; `pageSize` is clamped server-side to
   * 1..200 (default 50); `afterCursor` is an opaque value returned by
   * a previous call. Rejects on stanza error so the UI can render an
   * explicit failure state.
   */
  async adminUsersList(opts: { prefix?: string | null; pageSize?: number | null; afterCursor?: string | null } = {}): Promise<AdminUsersPage> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_users_list) {
      throw new Error("admin_users_list binding missing — server does not support admin V1");
    }
    return await xmpp.admin_users_list(opts.prefix ?? null, opts.pageSize ?? null, opts.afterCursor ?? null);
  }
  /**
   * Admin V2 spaces: list. Wraps `WaddleClient.admin_spaces_list`.
   *
   * The wasm method consumes a serde-typed `WaddleAdminSpacesListArgs`
   * struct, so this wrapper must pass snake_case keys verbatim.
   */
  async adminSpacesList(opts: { prefix?: string | null; pageSize?: number | null; afterCursor?: string | null } = {}): Promise<WasmAdminSpacesListResult> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_spaces_list) {
      throw new Error("admin_spaces_list binding missing — server does not support admin V2");
    }
    return await xmpp.admin_spaces_list({
      prefix: opts.prefix ?? null,
      page_size: opts.pageSize ?? null,
      after_cursor: opts.afterCursor ?? null,
    });
  }
  async adminSpacesCreate(opts: { name: string; description?: string | null; iconUrl?: string | null }): Promise<WasmAdminSpaceRef> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_spaces_create) throw new Error("admin_spaces_create binding missing");
    return await xmpp.admin_spaces_create({
      name: opts.name,
      description: opts.description ?? null,
      icon_url: opts.iconUrl ?? null,
    });
  }
  async adminSpacesUpdate(opts: { spaceJid: string; name?: string | null; description?: string | null; iconUrl?: string | null }): Promise<WasmAdminSpaceRef> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_spaces_update) throw new Error("admin_spaces_update binding missing");
    return await xmpp.admin_spaces_update({
      space_jid: opts.spaceJid,
      name: opts.name ?? null,
      description: opts.description ?? null,
      icon_url: opts.iconUrl ?? null,
    });
  }
  async adminSpacesDelete(opts: { spaceJid: string }): Promise<boolean> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_spaces_delete) throw new Error("admin_spaces_delete binding missing");
    return await xmpp.admin_spaces_delete({ space_jid: opts.spaceJid, confirm: "yes" });
  }
  async adminSpacesMembers(opts: { spaceJid: string; pageSize?: number | null; afterCursor?: string | null }): Promise<WasmAdminSpacesMembersResult> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_spaces_members) throw new Error("admin_spaces_members binding missing");
    return await xmpp.admin_spaces_members({
      space_jid: opts.spaceJid,
      page_size: opts.pageSize ?? null,
      after_cursor: opts.afterCursor ?? null,
    });
  }
  async adminSpacesSetRole(opts: { spaceJid: string; memberJid: string; role: "owner" | "admin" | "member" | "none" }): Promise<WasmAdminSpacesSetRoleResult> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_spaces_set_role) throw new Error("admin_spaces_set_role binding missing");
    return await xmpp.admin_spaces_set_role({
      space_jid: opts.spaceJid,
      member_jid: opts.memberJid,
      role: opts.role,
    });
  }
  async adminChannelsList(opts: { spaceJid?: string | null; prefix?: string | null; pageSize?: number | null; afterCursor?: string | null } = {}): Promise<WasmAdminChannelsListResult> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_channels_list) throw new Error("admin_channels_list binding missing");
    return await xmpp.admin_channels_list({
      space_jid: opts.spaceJid ?? null,
      prefix: opts.prefix ?? null,
      page_size: opts.pageSize ?? null,
      after_cursor: opts.afterCursor ?? null,
    });
  }
  async adminChannelsCreate(opts: { name: string; topic?: string | null; spaceJid?: string | null; isPublic?: boolean | null }): Promise<WasmAdminChannelRef> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_channels_create) throw new Error("admin_channels_create binding missing");
    return await xmpp.admin_channels_create({
      name: opts.name,
      topic: opts.topic ?? null,
      space_jid: opts.spaceJid ?? null,
      is_public: opts.isPublic ?? null,
    });
  }
  async adminChannelsUpdate(opts: { channelJid: string; name?: string | null; topic?: string | null; isPublic?: boolean | null }): Promise<WasmAdminChannelRef> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_channels_update) throw new Error("admin_channels_update binding missing");
    return await xmpp.admin_channels_update({
      channel_jid: opts.channelJid,
      name: opts.name ?? null,
      topic: opts.topic ?? null,
      is_public: opts.isPublic ?? null,
    });
  }
  async adminChannelsDelete(opts: { channelJid: string }): Promise<boolean> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_channels_delete) throw new Error("admin_channels_delete binding missing");
    return await xmpp.admin_channels_delete({ channel_jid: opts.channelJid, confirm: "yes" });
  }
  async adminChannelsOccupants(opts: { channelJid: string; pageSize?: number | null; afterCursor?: string | null }): Promise<WasmAdminChannelsOccupantsResult> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_channels_occupants) throw new Error("admin_channels_occupants binding missing");
    return await xmpp.admin_channels_occupants({
      channel_jid: opts.channelJid,
      page_size: opts.pageSize ?? null,
      after_cursor: opts.afterCursor ?? null,
    });
  }
  async adminChannelsAffiliations(opts: { channelJid: string; filter?: string | null; pageSize?: number | null; afterCursor?: string | null }): Promise<WasmAdminChannelsAffiliationsResult> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_channels_affiliations) throw new Error("admin_channels_affiliations binding missing");
    return await xmpp.admin_channels_affiliations({
      channel_jid: opts.channelJid,
      filter: opts.filter ?? null,
      page_size: opts.pageSize ?? null,
      after_cursor: opts.afterCursor ?? null,
    });
  }
  async adminChannelsSetAffiliation(opts: { channelJid: string; memberJid: string; affiliation: "owner" | "admin" | "member" | "none" | "outcast"; reason?: string | null }): Promise<WasmAdminChannelsSetAffiliationResult> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_channels_set_affiliation) throw new Error("admin_channels_set_affiliation binding missing");
    return await xmpp.admin_channels_set_affiliation({
      channel_jid: opts.channelJid,
      member_jid: opts.memberJid,
      affiliation: opts.affiliation,
      reason: opts.reason ?? null,
    });
  }
  async adminChannelsKick(opts: { channelJid: string; occupantJid: string; reason?: string | null }): Promise<WasmAdminChannelsKickResult> {
    const xmpp = await this.requireConnectedXmpp();
    if (!xmpp.admin_channels_kick) throw new Error("admin_channels_kick binding missing");
    return await xmpp.admin_channels_kick({
      channel_jid: opts.channelJid,
      occupant_jid: opts.occupantJid,
      reason: opts.reason ?? null,
    });
  }
  async searchUsers(query: string): Promise<UserSearchResult[]> { if (!query.trim()) return []; const xmpp = await this.requireConnectedXmpp(); const users = await xmpp.search_users?.(query) as WasmUserSearchResult[]; return (users ?? []).map((user) => ({ id: user.jid, jid: user.jid, username: user.username ?? user.nick ?? user.jid.split("@")[0] ?? user.jid, display_name: user.display_name ?? user.name ?? null, avatar_url: null })); }
  async fetchUserAvatar(jid: string): Promise<string | null> {
    const xmpp = await this.requireConnectedXmpp();
    const bareJid = barePeerJid(jid);
    if (xmpp.request_avatar) {
      const avatar = await xmpp.request_avatar(bareJid) as WasmAvatar | null;
      if (avatar?.url) return avatar.url;
      if (avatar?.data) return avatarDataUrl(avatar.data, avatar.mime_type);
    }
    return null;
  }
  get agent(): XmppClientInstance | null { return this.xmpp; }

  private startSelfPing() { this.stopSelfPing(); this.selfPingTimer = setInterval(() => { void this.doSelfPing(); }, 60000); }
  private stopSelfPing() { if (this.selfPingTimer) { clearInterval(this.selfPingTimer); this.selfPingTimer = null; } }
  private async doSelfPing() { if (!this.xmpp?.send_raw_iq || !this.currentRoom) return; try { await this.xmpp.send_raw_iq(`<iq type="get" id="${crypto.randomUUID()}" to="${this.currentRoom}/${this.session.username}"><ping xmlns="urn:xmpp:ping"/></iq>`); } catch { this.roomDisconnectHandler?.(); } }
  private handleMessageAck(id: string) { const wasQueued = this.inflightQueuedIds.delete(id); if (wasQueued) removeQueuedMessage(this.queueScope, id); this.messageAckHandler?.(id); const pending = this.pendingSendAt.get(id); if (pending) { this.pendingSendAt.delete(id); this.fireHook(this.messageAckHooks, id, { kind: pending.kind, latencyMs: performance.now() - pending.at }); } if (wasQueued) this.emitQueueDepth(); }
  private handleMessageFailed(id: string) { const wasQueued = this.inflightQueuedIds.delete(id); this.messageDeliveryFailureHandler?.(id); const pending = this.pendingSendAt.get(id); if (pending) { this.pendingSendAt.delete(id); this.fireHook(this.messageFailHooks, id, { kind: pending.kind }); } if (wasQueued) this.emitQueueDepth(); }
  private handleSessionReady(xmpp: XmppClientInstance, lifecycle: SessionLifecycleEvent) {
    void this.runSessionReady(xmpp, lifecycle);
  }

  private async runSessionReady(xmpp: XmppClientInstance, lifecycle: SessionLifecycleEvent) {
    if (this.xmpp !== xmpp) return;
    this.connected = true; this.reconnectAttempt = 0;
    this.emitStatus({ state: "online", detail: countQueuedMessages(this.queueScope) > 0 ? lifecycle.type === "fresh" ? "Reconnected — replaying queued messages" : "Connection resumed — replaying queued messages" : lifecycle.type === "fresh" ? "Connection ready" : "Connection resumed" });
    // Coalesce duplicate triggers. Three event hooks call
    // `handleSessionReady` on the same xmpp handle; only the first
    // gets past this gate to run the per-session setup, lifecycle
    // emit, and catch-up. Subsequent triggers for the *same* xmpp
    // bail out silently.
    //
    // A barrier owned by a *different* xmpp handle (e.g. the old
    // handle from a Wi-Fi → cellular reconnect) does not block the
    // new handle — the new handle gets its own session-setup +
    // catch-up. The old barrier's `.finally` guards against writing
    // back into shared state when `this.xmpp` has moved on.
    if (this.resumeBarrier && this.resumeBarrier.xmpp === xmpp) return;
    if (lifecycle.type === "fresh") {
      this.inflightQueuedIds.clear();
      void this.enableCarbons(xmpp);
      // XEP-0490 §3.1 + §3.2: catch up displayed state and subscribe
      // to future +notify events. Both are best-effort and fully
      // fail-silent (chat works without MDS). Bound to the specific
      // xmpp reference so the bootstrap aborts cleanly if the client
      // disconnects before the catch-up IQ resolves.
      void this.bootstrapMdsDisplayed(xmpp);
    }
    const roomsToJoin = new Set([...this.retainedJoinedRoomJids, ...this.autoJoinRoomJids]);
    if (this.currentRoom) roomsToJoin.add(this.currentRoom);
    if (roomsToJoin.size > 0) {
      void this.fanOutAutoJoin([...roomsToJoin]);
    }
    this.emitSessionLifecycle(lifecycle);
    const catchupEntries = this.catchup.onSessionStarted();
    if (catchupEntries.length === 0) {
      this.flushAfterSessionReady(xmpp);
      this.fulfillOnceConnected();
      return;
    }
    // Build the barrier promise and the buffer *atomically* — set
    // both fields together before any `await` so a re-entrant
    // `handleSessionReady` (synchronous WASM callback) can never
    // observe `pendingDuringResume` set without `resumeBarrier`
    // also set.
    const catchupPromise = this.runReconnectCatchup(xmpp, catchupEntries);
    const barrierPromise = catchupPromise.finally(() => this.completeResumeBarrier(xmpp));
    this.pendingDuringResume = [];
    this.resumeBarrier = { xmpp, promise: barrierPromise };
    await barrierPromise;
    if (this.xmpp !== xmpp) return;
    this.fulfillOnceConnected();
  }

  /**
   * Resume-barrier completion: snapshot the buffer, atomically reset
   * `pendingDuringResume` + `resumeBarrier`, then flush the queue and
   * drain the buffer (in that order — see Bug 4 in PR2 plan).
   *
   * Extracted as a named private method so it can be unit-tested in
   * isolation; the in-line `.finally` callback was a source-text
   * grep target which made test brittle to formatting changes.
   *
   * Guards against `this.xmpp` having changed under us (mobile
   * disconnect mid-catchup) — in that case we still clean up our
   * own state but don't fire queue flush / buffer drain for the
   * stale handle.
   */
  private completeResumeBarrier(xmpp: XmppClientInstance) {
    const buffered = this.pendingDuringResume ?? [];
    // Only clear the barrier if it is still ours — a newer handle
    // may have replaced it after a full reconnect.
    if (this.resumeBarrier?.xmpp === xmpp) {
      this.resumeBarrier = null;
      this.pendingDuringResume = null;
    }
    if (this.xmpp !== xmpp) return;
    // Flush the locally-queued outbound *before* draining the live
    // buffer. Queued messages carry the user's send wall-clock from
    // before the pause; live arrivals from during the pause carry
    // later stamps. Flushing first means the cursor advances in
    // chronological order and `mergeLiveMessage` sees outbound
    // echoes alongside (or before) the inbound arrivals they belong
    // next to, instead of mis-ordering the tail (Bug 4).
    this.flushAfterSessionReady(xmpp);
    // Drain in arrival order. Each replay flows through the same
    // `dispatchLiveBodyMessage` path that a fresh socket arrival
    // would, so cursor advance + downstream `messageHandler` /
    // `directMessageHandler` dedup behave identically.
    for (const m of buffered) this.dispatchLiveBodyMessage(m);
  }

  private flushAfterSessionReady(xmpp: XmppClientInstance) {
    if (this.xmpp !== xmpp) return;
    void this.flushQueuedDirectMessages();
    const roomJid = this.currentRoom;
    if (roomJid) void this.flushQueuedRoomAfterJoin(xmpp, roomJid);
  }

  private async flushQueuedRoomAfterJoin(xmpp: XmppClientInstance, roomJid: string): Promise<void> {
    try {
      await this.ensureJoined(roomJid);
      if (this.xmpp !== xmpp) return;
      await this.flushQueuedRoomMessages(roomJid);
    } catch {
      // Join retry is driven by reconnect/session-ready and explicit sends.
    }
  }

  private fulfillOnceConnected() {
    if (!this.onceConnected) return;
    const done = this.onceConnected;
    this.onceConnected = null;
    this.onceConnectFailed = null;
    done();
  }

  private async bootstrapMdsDisplayed(xmpp: XmppClientInstance) {
    if (typeof xmpp.fetch_mds_displayed === "function") {
      try {
        const raw = await xmpp.fetch_mds_displayed();
        if (this.xmpp !== xmpp) return;
        const handler = this.mdsDisplayedHandler;
        if (handler && raw) {
          for (const entry of raw) {
            handler({ chatId: entry.chat_id, stanzaId: entry.stanza_id, stanzaIdBy: entry.stanza_id_by });
          }
        }
      } catch { /* best-effort */ }
    }
    if (this.xmpp !== xmpp) return;
    // XEP-0060 explicit subscribe so future publishes from another
    // resource fan out as headline events to this one.
    if (typeof xmpp.subscribe_mds_displayed === "function") {
      try { await xmpp.subscribe_mds_displayed(); } catch { /* best-effort */ }
    }
  }
  private handleDisconnected(xmpp: XmppClientInstance, error?: Error) {
    if (this.xmpp !== xmpp) return;
    const previouslyJoinedRooms = [...this.joinedMucs.keys()];
    this.connected = false; this.stopSelfPing(); this.xmpp = null;
    // The wire is gone — no point trying to send session-terminate.
    // Clear the local call slot so the UI doesn't strand on a stale
    // active overlay across reconnect; the reconnect path doesn't
    // re-establish call state (XEP-0353 has no resume semantics for
    // an in-flight call once the responder's connection drops).
    //
    // We MUST also drop the LiveKit room. The CallOverlay's
    // `onBeforeUnmount` is the usual route, but it won't fire when the
    // overlay re-renders to `phase: idle` and stays mounted (the parent
    // tree owns it) — the engine would keep an open SFU socket
    // pumping bytes until the next call replaced it. Tearing the
    // singleton engine down here is idempotent (no-op when nothing's
    // connected).
    clearCallState();
    clearMucCallParticipants();
    clearAllLiveCallParticipants();
    void useCallEngine().engine.disconnect();
    this.rejectRoomJoinWaiters(new Error("XMPP disconnected while joining a room"));
    this.retainedJoinedRoomJids = new Set([
      ...this.retainedJoinedRoomJids,
      ...previouslyJoinedRooms
        .map((roomJid) => roomJid.split("/")[0]?.trim().toLowerCase() ?? "")
        .filter(Boolean),
    ]);
    this.persistRetainedJoinedRooms();
    this.joinedMucs.clear();
    this.joinedMucReady.clear();
    this.clearRoomPresenceCaches();
    if (this.destroying) { this.clearResumeState(); this.emitStatus({ state: "offline", detail: error?.message ?? "Disconnected" }); return; }
    this.setResumeStateHandle(xmpp.get_resume_state_handle?.() ?? null);
    const resumeState = xmpp.get_resume_state?.() ?? null;
    this.resumeState = resumeState ? { ...resumeState, resource: this.resource } : null;
    // Keep transient reconnect state in this JS context only. The
    // shared per-account persisted SM slot is a pagehide handoff for
    // true tab replacement; writing it during ordinary disconnects
    // would let another live tab claim this same resource while this
    // client is still reconnecting with its in-memory handle.
    this.emitStatus({ state: "reconnecting", detail: countQueuedMessages(this.queueScope) > 0 ? "Connection lost — queued messages will send when reconnected" : (error?.message ?? "Connection lost, reconnecting...") });
    if (this.onceConnectFailed) { const fail = this.onceConnectFailed; this.onceConnected = null; this.onceConnectFailed = null; fail(error ?? new Error("XMPP connection failed")); }
    this.scheduleReconnect();
  }
  private handlePresence(presence: WasmPresence) {
    const from = presence.from ?? "";
    if ((presence as any).muc_jid !== undefined) {
      const [room, nick = ""] = from.split("/");
      const waiter = this.roomJoinWaiters.get(from);
      if (waiter && this.isOwnAvailableMucSelfPresence(presence)) waiter.resolve();
      if (!room) return;
      if (!nick && presence.vcard_avatar) this.roomAvatarHandler?.(room, presence.vcard_avatar);
      if (!nick) return;
      const roomHats = this.hatsFor(room);
      const roomAuthority = this.authorityFor(room);
      const roomPresence = this.presenceFor(room);
      const roomMemberJids = this.memberJidsFor(room);
      const isFocusedRoom = room === this.currentRoom;
      if (presence.presence_type === "unavailable") {
        if (this.isOwnMucSelfPresence(presence)) {
          this.revokeMucReadiness(room, { keepPendingJoin: this.roomJoinWaiters.has(from) });
        }
        delete roomHats[nick];
        delete roomAuthority[nick];
        roomPresence[nick] = "offline";
        delete roomMemberJids[nick];
        if (isFocusedRoom) {
          this.hatsHandler?.({ ...roomHats });
          this.authorityHandler?.({ ...roomAuthority });
          this.presenceHandler?.({ ...roomPresence });
          this.lastSeenHandler?.(nick, Date.now());
        }
        return;
      }
      // XEP-0317 hats are server-emitted descriptive metadata only.
      // No client-side fabrication from muc_affiliation/muc_role —
      // those flow as `roomAuthority` and drive authority chips
      // independently (see `parseMucAffiliation` / `parseMucRole`
      // below).
      roomHats[nick] = ((presence as any).hats ?? []).map((hat: any) => ({
        uri: hat.uri,
        title: hat.title,
      }));
      roomAuthority[nick] = {
        affiliation: parseMucAffiliation((presence as any).muc_affiliation),
        role: parseMucRole((presence as any).muc_role),
      };
      roomPresence[nick] = parsePresenceShow(presence.show);
      if ((presence as any).muc_jid) {
        const bare = barePeerJid((presence as any).muc_jid);
        roomMemberJids[nick] = bare;
        if (isFocusedRoom) this.memberJidHandler?.(nick, bare);
      }
      if (isFocusedRoom) {
        this.hatsHandler?.({ ...roomHats });
        this.authorityHandler?.({ ...roomAuthority });
        this.presenceHandler?.({ ...roomPresence });
      }
      return;
    }
    const bare = barePeerJid(from); if (!bare) return; if (presence.presence_type === "subscribe") return; this.presenceUpdateHandler?.({ bareJid: bare, show: mapPresenceShow(presence), ...(presence.status ? { status: presence.status } : {}) });
  }
  private handleMessage(message: WasmMessage & { carbon?: { sent?: boolean; received?: boolean }; inboxPush?: InboxEntry; _fromCarbon?: boolean }) {
    if (message.inboxPush) { this.inboxPushHandler?.(message.inboxPush); return; }
    if (message.id && this.carbonDedupIds.has(message.id) && !message._fromCarbon) {
      this.carbonDedupIds.delete(message.id);
      return;
    }
    if (message.call_event) {
      applyDmCallEvent({
        event: message.call_event,
        selfBareJid: barePeerJid(this.session.jid),
        selfFullJid: this.fullJid,
        to: message.to ?? message.call_event.to,
        timestamp: message.timestamp,
      });
      if (!message.body && !message.subject) return;
    }
    if (message.carbon?.sent || message.carbon?.received) return;
    if (message.chat_state && !message.body) {
      if (message.is_muc) { const roomJid = barePeerJid(message.from ?? message.to ?? ""); const nick = (message.from ?? "").split("/")[1] ?? "unknown"; if (roomJid === this.currentRoom && nick !== this.session.username) this.chatStateHandler?.({ roomJid, nick, state: message.chat_state as ChatStateType }); }
      else this.dmChatStateHandler?.({ peerJid: barePeerJid(message.from ?? message.to ?? ""), state: message.chat_state as ChatStateType });
      return;
    }
    if (message.displayed_marker_id) { if (message.is_muc) { const roomJid = barePeerJid(message.from ?? message.to ?? ""); const nick = (message.from ?? "").split("/")[1] ?? "unknown"; this.displayedHandler?.({ roomJid, nick, messageId: message.displayed_marker_id }); } else this.dmDisplayedHandler?.({ peerJid: barePeerJid(message.from ?? message.to ?? ""), messageId: message.displayed_marker_id }); return; }
    if (message.reaction_target_id) { if (message.is_muc) { const roomJid = barePeerJid(message.from ?? message.to ?? ""); const nick = (message.from ?? "").split("/")[1] ?? "unknown"; this.reactionHandler?.({ roomJid, nick, messageId: message.reaction_target_id, emojis: message.reaction_emojis }); } else { const fromBare = barePeerJid(message.from ?? ""); const toBare = barePeerJid(message.to ?? ""); const selfBare = barePeerJid(this.session.jid); const peerJid = fromBare === selfBare ? toBare : fromBare; const reactorJid = fromBare || selfBare; if (peerJid && reactorJid) this.dmReactionHandler?.({ peerJid, reactorJid, messageId: message.reaction_target_id, emojis: message.reaction_emojis }); } return; }
    if (message.pin_event && message.is_muc) {
      const roomJid = barePeerJid(message.from ?? message.to ?? "");
      this.pinEventHandler?.({ roomJid, event: message.pin_event });
      // Fall through: also render the system message in the timeline so
      // the user sees "alice pinned a message" inline (#414). The
      // pin store update has already happened above.
    }
    if (this.pendingDuringResume !== null) {
      this.pendingDuringResume.push(message);
      return;
    }
    this.dispatchLiveBodyMessage(message);
  }

  private dispatchLiveBodyMessage(message: WasmMessage & { carbon?: { sent?: boolean; received?: boolean }; inboxPush?: InboxEntry; _fromCarbon?: boolean }) {
    if (message.is_muc) {
      const converted = roomMessageFromArchived({ ...message, mam_id: message.id ?? crypto.randomUUID() } as WasmArchivedMessage, "live");
      if (!converted) return;
      this.catchup.recordRoomSeen(converted.roomJid, converted.createdAt, undefined, rawMessageSeenIds(message));
      if (converted.roomJid !== this.currentRoom && isRoomActivityMessage(converted)) { this.activityHandler?.(roomActivityEventFromMessage(converted)); return; }
      this.messageHandler?.(converted); return;
    }
    const converted = dmMessageFromArchived({ ...message, mam_id: message.id ?? crypto.randomUUID() } as WasmArchivedMessage, barePeerJid(this.session.jid), "live");
    if (converted) {
      this.catchup.recordDmSeen(converted.peerJid, converted.createdAt, undefined, rawMessageSeenIds(message));
      this.directMessageHandler?.(converted);
    }
  }
  private async runReconnectCatchup(
    xmpp: XmppClientInstance,
    entries: Array<{ kind: "dm" | "room"; key: string; after?: string; since?: string; seenIds?: string[] }>,
  ) {
    const sessionJid = this.session.jid;
    for (const entry of entries) {
      if (!this.isCurrentConnectedXmpp(xmpp, sessionJid)) return;
      try {
        if (entry.kind === "dm") {
          await this.runDmReconnectCatchup(xmpp, entry, sessionJid);
        } else {
          await this.runRoomReconnectCatchup(xmpp, entry, sessionJid);
        }
      } catch (error) {
        if (!this.isCurrentConnectedXmpp(xmpp, sessionJid)) return;
        this.emitError({
          kind: "history",
          recoverable: true,
          detail: `Reconnect catch-up failed for ${entry.key}`,
          cause: error,
        });
      }
    }
  }
  private async runDmReconnectCatchup(
    xmpp: XmppClientInstance,
    entry: { key: string; after?: string; since?: string; seenIds?: string[] },
    sessionJid: string,
  ) {
    if (!xmpp.fetch_dm_history_page) return;
    if (entry.after) {
      let after: string | undefined = entry.after;
      const seenAfter = new Set<string>();
      while (after) {
        if (seenAfter.has(after)) throw new Error(`Reconnect catch-up repeated archive cursor for ${entry.key}`);
        seenAfter.add(after);
        const page = await xmpp.fetch_dm_history_page(entry.key, 100, { type: "after", after }) as WasmMamPage;
        if (!this.isCurrentConnectedXmpp(xmpp, sessionJid)) return;
        const nextAfter = this.applyDmCatchupPage(page, undefined, entry.seenIds);
        if (isMamPageComplete(page)) return;
        if (!nextAfter) throw new Error(`Reconnect catch-up could not advance archive cursor for ${entry.key}`);
        after = nextAfter;
      }
      return;
    }
    const since = entry.since ?? this.catchup.getDmLastSeen(entry.key);
    if (!since) return;
    await this.runDmTimestampCatchup(xmpp, entry.key, since, sessionJid, entry.seenIds);
  }
  private async runRoomReconnectCatchup(
    xmpp: XmppClientInstance,
    entry: { key: string; after?: string; since?: string; seenIds?: string[] },
    sessionJid: string,
  ) {
    if (!xmpp.fetch_room_history_page) return;
    if (entry.after) {
      let after: string | undefined = entry.after;
      const seenAfter = new Set<string>();
      while (after) {
        if (seenAfter.has(after)) throw new Error(`Reconnect catch-up repeated archive cursor for ${entry.key}`);
        seenAfter.add(after);
        const page = await xmpp.fetch_room_history_page(entry.key, 100, { type: "after", after }) as WasmMamPage;
        if (!this.isCurrentConnectedXmpp(xmpp, sessionJid)) return;
        const nextAfter = this.applyRoomCatchupPage(page, undefined, entry.seenIds);
        if (isMamPageComplete(page)) return;
        if (!nextAfter) throw new Error(`Reconnect catch-up could not advance archive cursor for ${entry.key}`);
        after = nextAfter;
      }
      return;
    }
    const since = entry.since ?? this.catchup.getRoomLastSeen(entry.key);
    if (!since) return;
    await this.runRoomTimestampCatchup(xmpp, entry.key, since, sessionJid, entry.seenIds);
  }
  private async runDmTimestampCatchup(
    xmpp: XmppClientInstance,
    peerJid: string,
    since: string,
    sessionJid: string,
    seenIds?: ReadonlyArray<string>,
  ) {
    let pageParam: MamPageParam = { type: "latest" };
    const seenBefore = new Set<string>();
    const pages: WasmMamPage[] = [];
    while (true) {
      const page = await xmpp.fetch_dm_history_page?.(peerJid, 100, pageParam) as WasmMamPage;
      if (!this.isCurrentConnectedXmpp(xmpp, sessionJid)) return;
      if (page) pages.push(page);
      if (isMamPageComplete(page) || pageCrossesSince(page, since)) break;
      const firstArchiveId = pageFirstArchiveId(page);
      if (!firstArchiveId) throw new Error(`Reconnect catch-up could not page backward for ${peerJid}`);
      if (seenBefore.has(firstArchiveId)) throw new Error(`Reconnect catch-up repeated backward archive cursor for ${peerJid}`);
      seenBefore.add(firstArchiveId);
      pageParam = { type: "before", before: firstArchiveId };
    }
    const selfBare = barePeerJid(this.session.jid);
    for (const page of [...pages].reverse()) {
      this.applyDmCatchupPage(page, since, seenIds, { applyCallEvents: false });
    }
    for (const page of [...pages].reverse()) {
      this.applyDmCallEventsFromMamPage(page, selfBare, { since, seenIds });
    }
  }
  private async runRoomTimestampCatchup(
    xmpp: XmppClientInstance,
    roomJid: string,
    since: string,
    sessionJid: string,
    seenIds?: ReadonlyArray<string>,
  ) {
    let pageParam: MamPageParam = { type: "latest" };
    const seenBefore = new Set<string>();
    const pages: WasmMamPage[] = [];
    while (true) {
      const page = await xmpp.fetch_room_history_page?.(roomJid, 100, pageParam) as WasmMamPage;
      if (!this.isCurrentConnectedXmpp(xmpp, sessionJid)) return;
      if (page) pages.push(page);
      if (isMamPageComplete(page) || pageCrossesSince(page, since)) break;
      const firstArchiveId = pageFirstArchiveId(page);
      if (!firstArchiveId) throw new Error(`Reconnect catch-up could not page backward for ${roomJid}`);
      if (seenBefore.has(firstArchiveId)) throw new Error(`Reconnect catch-up repeated backward archive cursor for ${roomJid}`);
      seenBefore.add(firstArchiveId);
      pageParam = { type: "before", before: firstArchiveId };
    }
    for (const page of [...pages].reverse()) {
      this.applyRoomCatchupPage(page, since, seenIds);
    }
  }
  private applyDmCatchupPage(
    page: WasmMamPage | null | undefined,
    since?: string,
    seenIds?: ReadonlyArray<string>,
    options: { applyCallEvents?: boolean } = {},
  ): string | undefined {
    let lastArchiveId = pageLastArchiveId(page);
    const selfBare = barePeerJid(this.session.jid);
    if (options.applyCallEvents !== false) {
      this.applyDmCallEventsFromMamPage(page, selfBare, { since, seenIds });
    }
    for (const message of page?.messages ?? []) {
      const converted = dmMessageFromArchived(message, selfBare);
      if (!converted || shouldSkipCatchupMessage(converted, since, seenIds)) continue;
      this.catchup.recordDmSeen(converted.peerJid, converted.createdAt, converted.archiveId, messageSeenIds(converted));
      this.directMessageHandler?.(converted);
      lastArchiveId = converted.archiveId ?? lastArchiveId;
    }
    return lastArchiveId;
  }
  private applyRoomCatchupPage(page: WasmMamPage | null | undefined, since?: string, seenIds?: ReadonlyArray<string>): string | undefined {
    let lastArchiveId = pageLastArchiveId(page);
    for (const message of page?.messages ?? []) {
      const converted = roomMessageFromArchived(message);
      if (!converted || shouldSkipCatchupMessage(converted, since, seenIds)) continue;
      this.catchup.recordRoomSeen(converted.roomJid, converted.createdAt, converted.archiveId, messageSeenIds(converted));
      if (converted.roomJid !== this.currentRoom && isRoomActivityMessage(converted)) {
        this.activityHandler?.(roomActivityEventFromMessage(converted));
      } else {
        this.messageHandler?.(converted);
      }
      lastArchiveId = converted.archiveId ?? lastArchiveId;
    }
    return lastArchiveId;
  }
  private wireEvents(xmpp: XmppClientInstance & { enableKeepAlive?: (opts: { interval: number; timeout: number }) => void; disableKeepAlive?: () => void }) {
    if (!this.xmpp && !this.destroying) this.xmpp = xmpp;
    xmpp.set_on_connected?.(() => { if (!this.isCurrentXmpp(xmpp)) return; void this.enableCarbons(xmpp); });
    xmpp.set_on_session_lifecycle?.((event: string) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      if (event === "resumed") this.handleSessionReady(xmpp, { type: "resumed" }); else this.handleSessionReady(xmpp, { type: "fresh" });
    });
    xmpp.set_on_disconnected?.(() => this.handleDisconnected(xmpp));
    xmpp.set_on_error?.((detail: string) => {
      if (this.xmpp !== xmpp) return;
      this.emitError({ kind: "stream", recoverable: !this.destroying, detail });
    });
    xmpp.set_on_message?.((message: WasmMessage) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.handleMessage(message);
    });
    xmpp.set_on_presence?.((presence: WasmPresence) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.handlePresence(presence);
      // Side-effect track: MUC presence carrying the call extension
      // populates the per-room participants store so any consumer
      // (channel header, sidebar, list) can render "N in call"
      // without subscribing to the raw presence stream.
      applyMucCallPresence(presence);
    });
    xmpp.set_on_message_delivery_acked?.((id: string) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.handleMessageAck(id);
    });
    xmpp.set_on_message_delivery_failed?.((id: string) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.handleMessageFailed(id);
    });
    xmpp.set_on_mds_displayed?.((entry: WasmMdsDisplayedEntry) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.mdsDisplayedHandler?.({ chatId: entry.chat_id, stanzaId: entry.stanza_id, stanzaIdBy: entry.stanza_id_by });
    });
    xmpp.set_on_call?.((event: CallEvent) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      const prev = $callState.get();
      const selfBare = barePeerJid(this.session.jid);
      const isSelfOriginated = barePeerJid(event.from) === selfBare;
      const currentDmPeer =
        prev.phase === "incoming"
          ? prev.from
          : prev.phase === "outgoing"
            ? prev.to
            : prev.phase === "active" && prev.kind === "dm"
              ? prev.peer
              : undefined;
      if (
        event.kind === "propose" ||
        event.kind === "proceed" ||
        event.kind === "reject" ||
        event.kind === "retract" ||
        event.kind === "finish" ||
        (event.kind === "session-initiate" &&
          prev.phase === "incoming" &&
          prev.sid === event.sid) ||
        (event.kind === "session-accept" &&
          prev.phase === "outgoing" &&
          prev.sid === event.sid) ||
        (event.kind === "session-terminate" &&
          prev.phase === "active" &&
          prev.kind === "dm" &&
          prev.sid === event.sid)
      ) {
        applyDmCallEvent({
          event,
          selfBareJid: selfBare,
          selfFullJid: this.fullJid,
          to: event.to ?? currentDmPeer,
        });
      }
      const eventMatchesCurrentCall =
        "sid" in prev &&
        prev.sid === event.sid &&
        event.kind !== "propose";
      const selfOriginatedEventShouldTouchCurrentCall =
        eventMatchesCurrentCall &&
        (
          event.kind === "proceed" ||
          (event.kind === "reject" && prev.phase === "incoming") ||
          (event.kind === "session-initiate" && prev.phase === "incoming") ||
          (event.kind === "session-accept" && prev.phase === "outgoing") ||
          (event.kind === "session-terminate" && prev.phase === "active" && prev.kind === "dm") ||
          (event.kind === "finish" && prev.phase === "active" && prev.kind === "dm")
        );
      if (!isSelfOriginated || selfOriginatedEventShouldTouchCurrentCall) {
        applyCallEvent(event);
      }
      if (!isSelfOriginated) {
        void handleCallEventSideEffect(event, prev, xmpp as unknown as CallWireSender, this.fullJid);
      }
    });
    xmpp.on?.("session:started", () => {
      if (!this.isCurrentXmpp(xmpp)) return;
      xmpp.disableKeepAlive?.();
      xmpp.enableKeepAlive?.({ interval: 30, timeout: 15 });
      this.handleSessionReady(xmpp, { type: "fresh" });
    });
    xmpp.on?.("stream:management:resumed", () => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.handleSessionReady(xmpp, { type: "resumed" });
    });
    xmpp.on?.("disconnected", (error?: Error) => { xmpp.disableKeepAlive?.(); this.handleDisconnected(xmpp, error); });
    xmpp.on?.("message:acked", (msg: { id?: string }) => { if (!this.isCurrentXmpp(xmpp)) return; if (msg?.id) this.handleMessageAck(msg.id); });
    xmpp.on?.("message:failed", (msg: { id?: string }) => { if (!this.isCurrentXmpp(xmpp)) return; if (msg?.id) this.handleMessageFailed(msg.id); });
    xmpp.on?.("message", (message: WasmMessage) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.handleMessage(message);
    });
    xmpp.on?.("carbon:sent", (event: { carbon?: { forward?: { message?: WasmMessage } } }) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      const forwarded = event.carbon?.forward?.message; if (forwarded?.id) this.carbonDedupIds.add(forwarded.id); if (forwarded) this.handleMessage({ ...forwarded, _fromCarbon: true });
    });
    xmpp.on?.("carbon:received", (event: { carbon?: { forward?: { message?: WasmMessage } } }) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      const forwarded = event.carbon?.forward?.message; if (forwarded?.id) this.carbonDedupIds.add(forwarded.id); if (forwarded) this.handleMessage({ ...forwarded, _fromCarbon: true });
    });
    xmpp.on?.("presence", (presence: WasmPresence) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.handlePresence(presence);
      applyMucCallPresence(presence);
    });
  }
}
