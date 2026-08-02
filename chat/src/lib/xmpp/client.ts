import { withSpan } from "@/lib/telemetry";
import { stanzaErrorContext } from "@/lib/xmpp/stanza-error-context";
import { inferredFileDisposition, type ExtensionLaunchDescriptor } from "@/lib/chat-ui";
import type { ThreadsSort, ThreadsStatusFilter } from "@/lib/threads-view-filters";
import type { BroadcastShow } from "@/presence/effective-show";
import type { MemberSummary, UserSearchResult } from "../chat-types";
import type { WaddleSession } from "../server-auth";
import { $callState, applyCallEvent, clearCallState, tearDownActiveCall, type RawIqSender } from "@/lib/calls/call-store";
import { setMucCallHandRaised } from "@/lib/calls/muc-call-actions";
import {
  applyDmCallEvent,
  clearDmCallActivities,
} from "@/lib/calls/dm-call-activity";
import { handleCallEventSideEffect } from "@/lib/calls/call-effects";
import {
  applyMucCallPresence,
  clearMucCallParticipants,
} from "@/lib/calls/muc-call-presence";
import {
  applyRaisedHandPresence,
  clearAllRaisedHands,
} from "@/lib/calls/call-raised-hand";
import { applyMutePresence, clearAllMuted } from "@/lib/calls/call-mute";
import { clearAllLiveCallParticipants } from "@/lib/calls/muc-call-live-participants";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { createCallTransportRecovery } from "@/lib/calls/call-transport-recovery";
import type { CallWireSender } from "@/lib/calls/outbound";
import type { CallEvent, ExternalService } from "@/lib/calls/types";
import { coerceExternalServices } from "@/lib/calls/ice-servers";
import {
  isInCallReactionForActiveCall,
  receiveInCallReaction,
} from "@/lib/calls/in-call-reactions";
import { bareJidKey, barePeerJid, fullJidIdentityKey, jidDomain, jidLocalpart, resourceOf, roomBareJidFor } from "./jid";
import { TypedEventBus } from "./client-events";
import type {
  CatchupConversationFailure,
  CatchupHookInfo,
  ClientEvents,
  MdsDisplayedEntry,
  PubsubEvent,
  RoomAccessChangedEvent,
  StreamManagementTelemetry,
} from "./client-events";
import {
  OfflineSendQueue,
  ReconnectScheduler,
  ResumeStateStore,
  applyResumeStateToWasmConfig,
  browserOffline,
  compatWasmSendResult,
  isNonRetryableWasmSendFailure,
  type OutboundSendResult,
  type XmppResumeState,
  type XmppResumeStateHandle,
} from "./client-connection";
import {
  MamPager,
  isRoomActivityMessage,
  rawMessageSeenIds,
  roomActivityEventFromMessage,
  type DmCallActivityHydrationOptions,
} from "./client-mam";
import { MucAdmin, type AdminUsersPage } from "./client-muc-admin";
import { PresenceManager } from "./client-presence";
import { VCardManager } from "./client-vcard";
import {
  RoomJoinRetryCoordinator,
  type ScheduleRoomJoinRetryOptions,
} from "./room-join-retry";
import {
  PubsubManager,
  type DmBookmarkItem,
  type SetDmNotificationModeResult,
  type SetRoomNotificationModeOutcome,
  type UserBookmarkItem,
} from "./client-pubsub";
import type {
  ChatStateEvent,
  ChatStateType,
  DmChatStateEvent,
  DmDisplayedEvent,
  DmReactionEvent,
  DiscoveredChannel,
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
  RoomCatalogFingerprintField,
  RoomHats,
  RoomPresence,
  RosterContact,
  SessionLifecycleEvent,
  XmppErrorEvent,
  XmppStatusSnapshot,
} from "./types";
import { XMPP_STREAM_ERROR_CONDITIONS } from "./types";
import { prepareEncryptedAttachmentUpload } from "./encrypted-attachments";

import { discoverChannels, discoverTopology } from "./discovery";
import { discoverUploadService, uploadFile, type UploadProgress } from "./file-upload";
import { createGroupDm, type CreateGroupDmResult } from "./group-dm";
import {
  ReconnectCatchup,
  type DmConversationScope,
  type ReconnectCatchupEntry,
} from "./reconnect-catchup";
import {
  parseRegisterDeviceResult,
  parseRegisterPushDeviceRejection,
  type RegisterDeviceResult,
} from "./push-register-result";
import {
  createLocalStorageResumePersistence,
  type ResumePersistence,
} from "./resume-persistence";
import {
  hasCompleteRoomCatalogFingerprintAuthority,
  reconcileAutoJoinBlocks,
  ROOM_CATALOG_FINGERPRINT_FIELDS,
  roomCatalogFingerprint,
  terminalMucJoinCondition,
  type RoomAutoJoinBlock,
} from "./room-auto-join-policy";
import { clearDmCallJoinCacheForAccount } from "@/lib/calls/dm-call-join-cache";
import { clearMucCallSessionCacheForAccount } from "@/lib/calls/muc-call-session-cache";
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
import type { FeedEntry, FeedPostInput } from "./feed-types";
import type {
  Story,
  StoryPostInput,
  StoryReactionItem,
  StoryReactionSummary,
} from "./story-types";
import type { StoryReads } from "./story-reads-types";
import type { CommunityEvent, CommunityEventInput, PartStat } from "./event-types";
import type { FetchInboxOptions, InboxEntry, InboxResult } from "./inbox-types";
import type { ActivityPublication, MoodPublication, TunePublication, UserPepProfile } from "./pep-types";
import {
  type OutboundFileAttachment,
  type SendDirectMessageOptions,
  type SendGroupMessageOptions,
} from "./send-types";
import { requestPlaintextLinkPreviewLookup, trustedLinkPreviewMediaOrigin, type LinkPreviewLookupResult } from "./link-preview";
import {
  buildWasmSendOptions,
  dmMessageFromArchived,
  encodeBodyForSend,
  inboxEntryFromWasm,
  roomMessageFromArchived,
} from "./wasm-message-codecs";
import type {
  WasmAdminChannelRef,
  WasmAdminChannelType,
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
  WasmFetchThreadsOptions,
  WasmInboxConversation,
  WasmInboxResult,
  WasmMamPage,
  WasmMdsDisplayedEntry,
  WasmMessage,
  WasmPinEvent,
  WasmPresence,
  WasmPubsubEvent,
  WasmRoomMember,
  WasmRosterContact,
  WasmSendMessageOutcome,
  WasmServerVersion,
  WasmThreadsPage,
  WasmUserSearchResult,
} from "./wasm-types";
import type { VCard4Profile } from "./vcard4-types";
// Type-only: the status-preference wire shape (`{ mode, status? }`). The
// PresenceMode <-> wire mapping lives in the presence layer; this wrapper just
// carries the wire shape across the wasm boundary.
import type { StatusPreferenceWire } from "@/presence/status-preference";

export { dmMessageFromArchived, roomMessageFromArchived } from "./wasm-message-codecs";

type InboundWasmMessage = WasmMessage & {
  inboxPush?: InboxEntry;
  inbox_push?: WasmInboxConversation;
  /** Set by the carbon dedupe when a delay-carrying carbon completes a
   * direct-first pair: dispatch for timestamp upgrade only (no unread /
   * notification side effects downstream). */
  restampOnly?: true;
};

// Safety-net ceiling for a MUC join. XEP-0045 §7.2.2 has the service
// send the self-presence (status 110) only AFTER every existing
// occupant's presence, so a busy room or a slow link can legitimately
// take several seconds to deliver the roster. 110 is the definitive
// resolve signal; this timeout only bounds genuinely stuck joins.
const ROOM_SELF_PRESENCE_TIMEOUT_MS = 15_000;

type XmppStreamErrorPayload = string | {
  detail?: string | null;
  condition?: string | null;
  streamManagementError?: XmppErrorEvent["streamManagementError"] | null;
};

/**
 * #1164: stream-error conditions that no amount of retrying can fix —
 * the credentials or the session itself were rejected, so the honest
 * UI state is terminal `"error"` ("sign in again"), not an eternal
 * "reconnecting" spinner. Maps each condition to the user-facing
 * status detail (`connection-notice.ts` renders the copy).
 */
const TERMINAL_STREAM_CONDITION_DETAILS = {
  // RFC 6120 §4.9.3.12 / SASL failure surfaced as a stream error:
  // the session token is no longer accepted.
  "not-authorized": "Your session expired — sign in again to restore live messaging.",
  // RFC 6120 §4.9.3.3: the server closed this stream in favour of a
  // newer one for the same resource; reconnecting would just fight it.
  conflict: "This session was replaced by a newer sign-in for the same account.",
} as const;

function isTerminalStreamCondition(
  condition: string,
): condition is keyof typeof TERMINAL_STREAM_CONDITION_DETAILS {
  return condition in TERMINAL_STREAM_CONDITION_DETAILS;
}

function terminalDisconnectDetail(condition: string | undefined): string | null {
  if (!condition || !isTerminalStreamCondition(condition)) return null;
  return TERMINAL_STREAM_CONDITION_DETAILS[condition];
}

/**
 * The condition allowed to drive TERMINAL classification. Real stream
 * errors always arrive structured (`JsStreamError.condition` from the
 * WASM core); the only free-text payload that legitimately names a
 * terminal condition is waddle-xmpp-client's SASL ClientError display,
 * which backtick-quotes it ("… with condition `not-authorized`").
 * Loose word-matching (`streamErrorConditionFromText`) stays for the
 * emitted event's diagnostic `condition` field only — a benign error
 * whose text merely contains "conflict" must never latch terminal
 * state.
 */
function terminalConditionFromPayload(payload: XmppStreamErrorPayload): string | undefined {
  if (typeof payload !== "string") return payload.condition?.trim() || undefined;
  return /\bcondition `([a-z0-9-]+)`/.exec(payload)?.[1];
}

function streamErrorConditionFromText(text: string | null | undefined): string | undefined {
  const normalized = text?.trim().toLowerCase();
  if (!normalized) return undefined;
  if (XMPP_STREAM_ERROR_CONDITIONS.has(normalized)) return normalized;
  for (const condition of XMPP_STREAM_ERROR_CONDITIONS) {
    const escaped = condition.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    if (new RegExp(`(^|[^a-z0-9])${escaped}([^a-z0-9]|$)`).test(normalized)) {
      return condition;
    }
  }
  return undefined;
}

function normalizeXmppStreamErrorPayload(payload: XmppStreamErrorPayload): {
  detail: string;
  condition?: string;
  streamManagementError?: XmppErrorEvent["streamManagementError"];
} {
  if (typeof payload === "string") {
    const condition = streamErrorConditionFromText(payload);
    return {
      detail: payload,
      ...(condition ? { condition } : {}),
    };
  }
  const detail = payload.detail?.trim() || payload.condition?.trim() || "stream error";
  const condition = payload.condition?.trim() || streamErrorConditionFromText(detail);
  return {
    detail,
    ...(condition ? { condition } : {}),
    ...(payload.streamManagementError
      ? { streamManagementError: payload.streamManagementError }
      : {}),
  };
}

type WasmModule = typeof import("@waddle/xmpp-client-wasm");
type WasmClient = import("@waddle/xmpp-client-wasm").WaddleClient & {
  fetch_personal_history_page?: (max: number, pageParam: MamPageParam) => Promise<WasmMamPage>;
  discover_extension_routes?: () => Promise<unknown>;
  fetch_extension_route_items?: (route: unknown, roomJid: string) => Promise<unknown>;
};
type PagehideSmAckEnqueueOutcome = import("@waddle/xmpp-client-wasm").PagehideSmAckEnqueueOutcome;

// wasm-bindgen exports this C-style enum as stable numeric discriminants. Keep
// the browser boundary typed without importing the WASM module before init.
const PAGEHIDE_SM_ACK_SENT: PagehideSmAckEnqueueOutcome = 0;
const PAGEHIDE_SM_ACK_ALREADY_PENDING: PagehideSmAckEnqueueOutcome = 1;


/** Per-event handler signatures for the legacy emitter surface some
 * wasm builds expose alongside the `set_on_*` registration methods.
 * XEP-0280 carbons are NOT a separate event: the WASM core unwraps the
 * envelope and delivers the inner message through the normal `message`
 * path with a `carbon` direction marker (#1243). */
type CompatEmitterEvents = {
  "session:started": () => void;
  "stream:management:resumed": () => void;
  disconnected: (error?: Error) => void;
  "message:acked": (msg: { id?: string }) => void;
  "message:failed": (msg: { id?: string }) => void;
  message: (message: WasmMessage) => void;
  presence: (presence: WasmPresence) => void;
};

type CompatEmitter = {
  on?: <E extends keyof CompatEmitterEvents>(event: E, handler: CompatEmitterEvents[E]) => void;
};

type XmppClientInstance = Partial<WasmClient> & CompatEmitter & {
  joinRoom?: (roomJid: string, nick: string) => Promise<void>;
  leaveRoom?: (roomJid: string, nick: string) => Promise<void>;
  set_on_connected?: (cb: () => void) => void;
  set_on_disconnected?: (cb: () => void) => void;
  set_on_error?: (cb: (error: XmppStreamErrorPayload) => void) => void;
  set_on_message?: (cb: (message: WasmMessage) => void) => void;
  set_on_presence?: (cb: (presence: WasmPresence) => void) => void;
  set_on_message_delivery_acked?: (cb: (id: string) => void) => void;
  set_on_message_delivery_failed?: (cb: (id: string) => void) => void;
  set_on_call?: (cb: (event: CallEvent) => void) => void;
  set_on_session_lifecycle?: (cb: (event: string) => void) => void;
  set_on_mds_displayed?: (cb: (entry: WasmMdsDisplayedEntry) => void) => void;
  set_on_pubsub_event?: (cb: (event: WasmPubsubEvent) => void) => void;
  set_on_stream_management?: (cb: (event: StreamManagementTelemetry) => void) => void;
  send_in_call_reaction?: (to: string, type: "chat" | "groupchat", sid: string, emoji: string) => Promise<void>;
  get_resume_state?: () => XmppResumeState | null;
  get_resume_state_handle?: () => XmppResumeStateHandle | undefined;
  request_stream_management_ack?: () => Promise<void>;
  try_request_stream_management_ack_for_pagehide?: () => PagehideSmAckEnqueueOutcome;
  publish_mds_displayed?: (chatId: string, stanzaId: string, stanzaIdBy: string) => Promise<void>;
  supports_mds_publish_options?: () => Promise<boolean>;
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
  pin_direct_message?: (peerJid: string, targetStanzaId: string) => Promise<void>;
  unpin_direct_message?: (peerJid: string, targetStanzaId: string) => Promise<void>;
  fetch_room_messages_by_stanza_ids?: (
    targetJid: string,
    stanzaIds: string[],
  ) => Promise<import("./wasm-types").WasmMamPage | null>;
  fetch_direct_messages_by_stanza_ids?: (
    peerJid: string,
    stanzaIds: string[],
  ) => Promise<import("./wasm-types").WasmMamPage | null>;
};

let wasmModulePromise: Promise<WasmModule> | null = null;

function createXmppResource() {
  const randomId = globalThis.crypto?.randomUUID?.() ?? fallbackUuid();
  return `web-${randomId}`;
}

function fallbackUuid(randomBytes?: Uint8Array): string {
  const bytes = randomBytes ? new Uint8Array(randomBytes) : new Uint8Array(16);
  if (bytes.length !== 16) throw new Error("UUID fallback requires 16 random bytes");
  if (!randomBytes) {
    if (globalThis.crypto?.getRandomValues) {
      globalThis.crypto.getRandomValues(bytes);
    } else {
      for (let index = 0; index < bytes.length; index += 1) {
        bytes[index] = Math.floor(Math.random() * 256);
      }
    }
  }
  bytes[6] = (bytes[6]! & 0x0f) | 0x40;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0"));
  return `${hex.slice(0, 4).join("")}-${hex.slice(4, 6).join("")}-${hex.slice(6, 8).join("")}-${hex.slice(8, 10).join("")}-${hex.slice(10).join("")}`;
}

/** For tests only — prove the no-randomUUID path remains UUID-shaped. */
export function __createFallbackXmppResourceForTesting(bytes: Uint8Array): string {
  return `web-${fallbackUuid(bytes)}`;
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


export class BrowserXmppClient {
  private session: WaddleSession;
  private get queueScope() { return barePeerJid(this.session.jid); }
  private readonly resource: string;
  private readonly events = new TypedEventBus<ClientEvents>();
  private xmpp: XmppClientInstance | null = null;
  private mdsPublishOptionsSupport: Promise<boolean> | null = null;
  private connectPromise: Promise<void> | null = null;
  private connected = false;
  private destroying = false;
  private readonly callTransportRecovery = createCallTransportRecovery({
    isCallMediaActive: () => $callState.get().phase === "active",
    teardown: () => this.teardownCallAfterTransportLoss(),
  });
  /** Terminal lifecycle latch. A disposed client is never reconnectable. */
  private disposed = false;
  private disposePromise: Promise<void> | null = null;
  private currentRoom: string | null = null;
  private roomSwitchPromise: Promise<void> | null = null;
  private roomSwitchTarget: string | null = null;
  private selfPingTimer: ReturnType<typeof setInterval> | null = null;
  private readonly joinedMucs = new Map<string, Promise<void>>();
  private readonly joinedMucJoinTokens = new Map<string, symbol>();
  private readonly joinedMucReady = new Set<string>();
  private retainedJoinedRoomJids = new Set<string>();
  private autoJoinRoomJids: ReadonlyArray<string> = [];
  // Self-presence-confirmed (XEP-0045 status 110) room keys captured at
  // the last disconnect. A `resumed` session-ready re-seeds the join
  // trackers from this without sending presence — MUC occupancy survives
  // the SM detach-for-resume server-side (#1221). Cleared on logout.
  private resumedSessionRoomKeys = new Set<string>();
  // Canonical room keys `fanOutAutoJoin` has attempted this session epoch
  // (#1221). Bounds each room to one auto-join attempt per epoch — a
  // failed join (e.g. 15s self-presence timeout) is not retried by a
  // later trigger, and the three fan-out triggers per session coalesce.
  // Cleared on disconnect so a genuine fresh cycle rejoins.
  private readonly autoJoinAttemptedRoomKeys = new Set<string>();
  // XEP-0045 terminal room-entry denials are not transient join failures:
  // reconnecting cannot grant membership or undo a ban. Keep those rooms
  // out of automatic fan-out while still allowing `switchRoom` to retry
  // explicitly when the user navigates back.
  private terminallyDeniedAutoJoinRooms = new Map<string, RoomAutoJoinBlock>();
  private currentRoomCatalogFingerprintEvidence = new Map<
    string,
    Pick<RoomAutoJoinBlock, "catalogFingerprint" | "catalogFingerprintFields">
  >();
  private roomDiscoveryGeneration = 0;
  private uploadServiceJid: string | null = null;
  private mucServiceJid = "";
  private discoveredRoomJids = new Map<string, string>();
  private readonly reconnect: ReconnectScheduler;
  private readonly resume: ResumeStateStore;
  private readonly outboundQueue: OfflineSendQueue;
  private readonly mam: MamPager;
  private readonly mucAdmin: MucAdmin;
  private readonly presence: PresenceManager;
  private readonly pubsub: PubsubManager;
  private readonly vcard: VCardManager;
  private readonly roomJoinWaiters = new Map<
    string,
    { promise: Promise<void>; requestedNick: string; resolve: () => void; reject: (error: Error) => void }
  >();
  private roomJoinRetry = new RoomJoinRetryCoordinator();
  // XEP-0280 dedupe memory: key → which representation was seen first
  // ("carbon" or "direct"). The source disambiguates a replayed carbon
  // (drop) from a carbon completing a direct-first pair (pass through
  // for its authoritative forwarded <delay/>).
  private readonly carbonDedupIds = new Map<string, "carbon" | "direct">();
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
  // Targeted follow-ups — XEP-0333 displayed markers and XEP-0444
  // reactions — are buffered too (#1165): applying one before its
  // target row exists makes the merge layer drop it silently. The
  // drain dispatches bodies first, then follow-ups, preserving
  // arrival order within each kind. Ephemeral traffic (chat states,
  // in-call reactions, pin-event store updates) stays live — it is
  // meaningless to replay after the fact.
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
  // The handle whose `runSessionReady` has already run this session
  // (#1221). Latches per-handle so the three duplicate session-ready
  // hooks coalesce even on the no-catch-up path (which opens no barrier).
  // Reset on disconnect so the next handle runs its own setup.
  private sessionReadyHandledXmpp: XmppClientInstance | null = null;
  private pendingDuringResume: InboundWasmMessage[] | null = null;
  // F2: buffer entries stranded by a barrier that failed mid-catch-up
  // (the connection died, so `completeResumeBarrier` could not drain
  // them). Those stanzas were SM-acked — the server will never replay
  // them, and a MAM refetch surfaces reactions/markers as ignored
  // message rows — so they are carried into the NEXT barrier's buffer
  // (arrival order preserved) and drained when it completes.
  private carriedPendingDuringResume: InboundWasmMessage[] = [];
  // #1164: set when a stream error announced a terminal condition
  // (see `TERMINAL_STREAM_CONDITION_DETAILS`). The wire error and the
  // disconnect arrive as two separate WASM callbacks; this carries
  // the classification from the first into `handleDisconnected`,
  // which then emits `state: "error"` instead of scheduling retries.
  // Cleared by a user-explicit fresh attempt (`connectWithFreshBudget`).
  private terminalDisconnectDetail: string | null = null;
  // F1: sticky marker that the client surfaced a terminal `state:
  // "error"` ("sign in again"). While set, internal/background
  // `connect()` calls reject fast without scheduling — only a
  // user-explicit recovery intent (`connectWithFreshBudget`, or a
  // page load constructing a new client) may leave this state.
  private inTerminalErrorState = false;

  constructor(session: WaddleSession, persistence?: ResumePersistence) {
    this.session = session;
    this.mucServiceJid = `muc.${jidDomain(session.jid)}`;
    // Per-account so a logout/login on the same browser doesn't mix
    // cursors. `session.jid` is the bare JID — already unique per
    // identity — and matches the `accountKey` used by the outbound
    // queue store.
    this.resumePersistence = persistence ?? createLocalStorageResumePersistence(session.jid);
    this.catchup = new ReconnectCatchup(this.resumePersistence);
    this.resume = new ResumeStateStore(this.resumePersistence);
    // Restore any XEP-0198 resume state persisted by a prior tab
    // session. If a resume-state handle is also recovered via the
    // WASM client (live, same JS context), that takes precedence in
    // `doConnect`. The POD resume state is the only piece that
    // survives a full page reload, so hydrate it eagerly.
    const restored = this.resume.consumePersisted();
    this.resource = restored?.resource || createXmppResource();
    this.outboundQueue = new OfflineSendQueue({
      queueScope: () => this.queueScope,
      events: this.events,
      canUseConnectedSession: () => this.canUseConnectedSession(),
      roomIsReady: (roomJid) => this.roomIsReady(roomJid),
      enqueueReason: () => this.enqueueReason(),
      emitStatus: (snapshot) => this.emitStatus(snapshot),
      roomMemberJids: (roomJid) => this.memberJidsFor(roomJid),
      sendDirect: (peerJid, body, opts) => this.sendQueuedDirectMessage(peerJid, body, opts),
      sendRoom: (roomJid, body, opts) => this.sendQueuedRoomMessage(roomJid, body, opts),
    });
    this.outboundQueue.seedFromResumeState(restored);
    this.mam = new MamPager({
      sessionJid: () => this.session.jid,
      fullJid: () => this.fullJid,
      trustedMediaOrigin: () => this.trustedLinkPreviewMediaOrigin(),
      currentRoom: () => this.currentRoom,
      catchup: this.catchup,
      events: this.events,
      emitError: (event) => this.emitError(event),
      requireConnectedXmpp: () => this.requireConnectedXmpp(),
      ensureRoomReady: async (spaceId, channelId) => {
        await this.connect();
        await this.switchRoom(spaceId, channelId);
      },
      roomJidForChannel: (channelId) => this.roomJidForChannel(channelId),
      isCurrentConnected: (xmpp, sessionJid) =>
        this.xmpp === xmpp && this.connected && !this.destroying && this.session.jid === sessionJid,
      // XEP-0045 §7.5 (#1256): archived/catch-up DM re-emissions get the
      // same occupant classification as the live path, so a MUC PM never
      // re-files under the room bare JID after a reconnect.
      classifyMucPm: (message) => this.mucPmOccupant(message),
      isMucPmPeer: (peerJid) => this.isMucPmPeer(peerJid),
    });
    this.mucAdmin = new MucAdmin({
      requireConnectedXmpp: () => this.requireConnectedXmpp(),
      roomJidForChannel: (channelId) => this.roomJidForChannel(channelId),
      emitError: (event) => this.emitError(event),
    });
    this.pubsub = new PubsubManager({
      sessionJid: () => this.session.jid,
      requireConnectedXmpp: () => this.requireConnectedXmpp(),
    });
    this.vcard = new VCardManager({
      requireConnectedXmpp: () => this.requireConnectedXmpp(),
    });
    this.presence = new PresenceManager({
      events: this.events,
      currentRoom: () => this.currentRoom,
      ownFullJidCandidates: () => this.ownFullJidCandidates(),
      requireConnectedXmpp: () => this.requireConnectedXmpp(),
      handleMucPresenceError: (presence) => this.handleMucPresenceError(presence),
      onOwnSelfPresence: (roomJid) => {
        this.roomJoinWaiters.get(this.roomJoinKey(roomJid))?.resolve();
        this.markMucReadyFromSelfPresence(roomJid);
      },
      onOwnUnavailable: (roomJid) => {
        this.revokeMucReadiness(roomJid, {
          keepPendingJoin: this.roomJoinWaiters.has(this.roomJoinKey(roomJid)),
        });
      },
    });
    this.reconnect = new ReconnectScheduler({
      isDestroying: () => this.destroying,
      connect: () => this.connectFromScheduler(),
      onScheduled: (info) => this.events.emitSafe("reconnectScheduled", info),
      // #1164: the retry budget ran out — an endless "reconnecting"
      // banner would be dishonest. Only a user-explicit recovery
      // restores the budget: the network-return `online` event fires
      // `connectWithFreshBudget`, and a page reload constructs a new
      // client. Internal `connect()` calls reject while exhausted and
      // never restart the loop (F1).
      onExhausted: () => {
        this.armOnlineRecovery();
        if (browserOffline()) {
          // C2: the truthful state is offline, not a terminal error —
          // and the armed `online` listener retries when the network
          // returns instead of demanding a reload.
          this.emitStatus({
            state: "offline",
            detail: "You're offline — reconnecting when the network returns.",
          });
          return;
        }
        this.emitStatus({
          state: "error",
          detail: "We couldn't reconnect after several attempts. Check your connection, then reload to try again.",
        });
      },
    });
    this.retainedJoinedRoomJids = new Set(this.resumePersistence.loadJoinedRooms());
    for (const block of this.resumePersistence.loadAutoJoinBlocks?.() ?? []) {
      const key = this.roomJoinKey(block.roomJid);
      if (key) this.terminallyDeniedAutoJoinRooms.set(key, { ...block, roomJid: key });
    }
  }

  private trustedLinkPreviewMediaOrigin(): string | null {
    return trustedLinkPreviewMediaOrigin(this.session);
  }

  /** Full JID (`bare/resource`) for this session. Needed by the
   * call layer when constructing Jingle session-initiate / accept,
   * which must address the peer's full JID and stamp our own. */
  get fullJid(): string { return `${this.session.jid}/${this.resource}`; }

  /** Random resource bound by this browser session; safe telemetry correlation id. */
  get xmppResource(): string { return this.resource; }
  /** Bare JID for this session. */
  get bareJid(): string { return this.session.jid; }

  private isCurrentXmpp(xmpp: XmppClientInstance): boolean {
    return this.xmpp === xmpp && !this.destroying && !this.disposed;
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

  /**
   * Normalized key under which a room's join waiter is registered.
   * Keying on the bare room JID (case-folded) — not the full
   * `room/nick` occupant JID — makes join resolution independent of
   * the nick the service echoes back and of localpart case.
   */
  private roomJoinKey(roomJid: string): string {
    return roomJid.split("/")[0]?.trim().toLowerCase() ?? "";
  }

  private roomIsTerminallyDenied(roomJid: string): boolean {
    return this.terminallyDeniedAutoJoinRooms.has(this.roomJoinKey(roomJid));
  }

  private markMucReadyFromSelfPresence(roomJid: string): void {
    const key = this.roomJoinKey(roomJid);
    if (!key) return;
    this.roomJoinRetry.complete(key);
    if (!this.joinedMucs.has(key)) {
      this.joinedMucs.set(key, Promise.resolve());
    }
    this.joinedMucReady.add(key);
    this.rememberJoinedRoom(roomJid);
  }

  /**
   * Restore MUC join state after an SM resume without touching the wire
   * (#1221). Occupancy survived the detach-for-resume server-side, so we
   * mark each snapshotted room ready (resolved join, no presence sent)
   * and record it as attempted so a concurrent `discoverTopology`
   * fan-out does not re-send a join either.
   */
  private reseedResumedRooms(): void {
    for (const key of this.resumedSessionRoomKeys) {
      if (!key) continue;
      if (!this.joinedMucs.has(key)) this.joinedMucs.set(key, Promise.resolve());
      this.joinedMucReady.add(key);
      this.autoJoinAttemptedRoomKeys.add(key);
    }
    // One-shot: the snapshot is a handoff from the last disconnect to
    // this resume. Clear it once consumed so a later `.size` check can
    // never read a stale snapshot (the next disconnect repopulates it).
    this.resumedSessionRoomKeys.clear();
  }

  /**
   * Fan out MUC joins for the retained + autojoin rooms plus the focused
   * room (#1221). Used on a fresh bind and on a resume whose readiness
   * snapshot was lost. Single-flight per epoch via `fanOutAutoJoin`.
   */
  private fanOutJoinableRooms(): void {
    const roomsToJoin = new Set([...this.retainedJoinedRoomJids, ...this.autoJoinRoomJids]);
    if (this.currentRoom) roomsToJoin.add(this.currentRoom);
    if (roomsToJoin.size > 0) void this.fanOutAutoJoin([...roomsToJoin]);
  }

  private handleMucPresenceError(presence: WasmPresence): boolean {
    if (presence.presence_type !== "error") return false;
    const [rawRoom = "", errorNick = ""] = presence.from?.split("/") ?? [];
    const room = rawRoom.trim();
    const key = this.roomJoinKey(room);
    if (!room || !key) return false;

    const waiter = this.roomJoinWaiters.get(key);
    if (!waiter) return false;
    if (errorNick !== waiter.requestedNick) return false;

    const terminalAuthorizationCondition = terminalMucJoinCondition(
      presence.error_type,
      presence.error_condition,
    );
    if (presence.error_type === "wait") this.scheduleRoomJoinRetry(room);
    const rejection = new Error(
      terminalAuthorizationCondition
        ? "You need access to this channel."
        : "Channel presence was rejected. Try again in a moment.",
    );
    waiter.reject(rejection);
    // Fully revoke — including the pending joinedMucs promise and its
    // token — BEFORE emitting, so an error listener that synchronously
    // retries the join starts a fresh one instead of awaiting the doomed
    // rejected entry (ensureJoined's own catch cleanup only lands several
    // microtasks later and its token guard makes this early delete safe).
    this.revokeMucReadiness(room);
    if (terminalAuthorizationCondition) {
      this.roomJoinRetry.reset(key);
      const catalogFingerprintEvidence =
        this.currentRoomCatalogFingerprintEvidence.get(key);
      this.terminallyDeniedAutoJoinRooms.set(key, {
        roomJid: key,
        condition: terminalAuthorizationCondition,
        ...catalogFingerprintEvidence,
      });
      this.persistAutoJoinBlocks();
      this.events.emit("roomAccessChanged", {
        roomJid: key,
        state: "required",
        condition: terminalAuthorizationCondition,
      });
      this.resumedSessionRoomKeys.delete(key);
      if (this.retainedJoinedRoomJids.delete(key)) this.persistRetainedJoinedRooms();
    }
    this.emitError({
      kind: "muc-join",
      recoverable: !terminalAuthorizationCondition,
      detail: `room join rejected — ${room}`,
      cause: rejection,
      roomLocalpart: jidLocalpart(room),
      ...stanzaErrorContext({
        condition: presence.error_condition,
        errorType: presence.error_type,
        text: presence.error_text,
      }),
    });
    return true;
  }

  private revokeMucReadiness(roomJid: string, options: { keepPendingJoin?: boolean } = {}): void {
    const key = this.roomJoinKey(roomJid);
    if (!key) return;
    if (!options.keepPendingJoin) {
      this.joinedMucs.delete(key);
      this.joinedMucJoinTokens.delete(key);
    }
    this.joinedMucReady.delete(key);
  }

  setMessageHandler(h: (message: LiveRoomMessage) => void) { this.events.set("message", h); }
  /** #414: receive `<pin-event/>` system messages from a room. */
  setPinEventHandler(h: (event: { roomJid: string; event: WasmPinEvent }) => void) { this.events.set("pinEvent", h); }
  setDirectMessageHandler(h: (message: LiveDmMessage) => void) { this.events.set("directMessage", h); }
  setStatusHandler(h: (status: XmppStatusSnapshot) => void) { this.events.set("status", h); }
  setChatStateHandler(h: (event: ChatStateEvent) => void) { this.events.set("chatState", h); }
  setDmChatStateHandler(h: (event: DmChatStateEvent) => void) { this.events.set("dmChatState", h); }
  setReactionHandler(h: (event: ReactionEvent) => void) { this.events.set("reaction", h); }
  setDmReactionHandler(h: (event: DmReactionEvent) => void) { this.events.set("dmReaction", h); }
  setDisplayedHandler(h: (event: { roomJid: string; nick: string; messageId: string }) => void) { this.events.set("displayed", h); }
  setDmDisplayedHandler(h: (event: DmDisplayedEvent) => void) { this.events.set("dmDisplayed", h); }
  setPresenceUpdateHandler(h: (event: PresenceUpdateEvent) => void) { this.events.set("presenceUpdate", h); }
  setMemberJidHandler(h: (nick: string, bareJid: string) => void) { this.events.set("memberJid", h); }
  setHatsHandler(h: (hats: RoomHats) => void) { this.events.set("hats", h); }
  setAuthorityHandler(h: (authority: RoomAuthority) => void) { this.events.set("authority", h); }
  setActivityHandler(h: (event: RoomActivityEvent) => void) { this.events.set("activity", h); }
  setInboxPushHandler(h: (entry: InboxEntry) => void) { this.events.set("inboxPush", h); }
  setRoomAvatarHandler(h: (roomJid: string, hash: string) => void) { this.events.set("roomAvatar", h); }
  setRoomDisconnectHandler(h: () => void) { this.events.set("roomDisconnect", h); }
  setPresenceHandler(h: (presence: RoomPresence) => void) { this.events.set("presence", h); }
  setLastSeenHandler(h: (nick: string, timestamp: number) => void) { this.events.set("lastSeen", h); }
  setMessageAckHandler(h: (messageId: string) => void) { this.events.set("messageAck", h); }
  setMessageDeliveryFailureHandler(h: (messageId: string) => void) { this.events.set("messageDeliveryFailure", h); }
  setQueuedMessageStatusHandler(h: (messageId: string, status: "queued" | "sending") => void) { this.events.set("queuedMessageStatus", h); }
  setSessionLifecycleHandler(h: (event: SessionLifecycleEvent) => void) { this.events.set("sessionLifecycle", h); }
  setCatchupFailureHandler(h: (failure: CatchupConversationFailure) => void) { this.events.set("catchupFailure", h); }

  onMessageAcked(hook: (id: string, meta: { kind: "room" | "dm"; latencyMs: number }) => void) { this.events.on("messageAcked", hook); }
  onMessageDeliveryFailed(hook: (id: string, meta: { kind: "room" | "dm" }) => void) { this.events.on("messageDeliveryFailed", hook); }
  onSessionLifecycle(hook: (event: SessionLifecycleEvent) => void) { this.events.on("sessionLifecycleHook", hook); }
  onStatus(hook: (status: XmppStatusSnapshot, meta: { reconnectDurationMs?: number }) => void) { this.events.on("statusHook", hook); }
  onSendEnqueued(hook: (info: { kind: "room" | "dm"; reason: string }) => void) { this.events.on("sendEnqueued", hook); }
  onQueueDepthChange(hook: (depth: { kind: "room" | "dm"; persisted: number; inflight: number }) => void) { this.events.on("queueDepthChange", hook); }
  onError(hook: (event: XmppErrorEvent) => void) { this.events.on("error", hook); }
  listRoomAccessRequirements(): ReadonlyArray<Extract<RoomAccessChangedEvent, { state: "required" }>> {
    return [...this.terminallyDeniedAutoJoinRooms.values()].map((block) => ({
      roomJid: block.roomJid,
      state: "required",
      condition: block.condition,
    }));
  }
  onRoomAccessChanged(hook: (event: RoomAccessChangedEvent) => void) { return this.events.on("roomAccessChanged", hook); }
  onReconnectScheduled(hook: (info: { attempt: number; delayMs: number }) => void) { this.events.on("reconnectScheduled", hook); }
  onCatchup(hook: (info: CatchupHookInfo) => void) { this.events.on("catchup", hook); }
  onResumeDrain(hook: (info: { buffered: number; durationMs: number }) => void) { this.events.on("resumeDrain", hook); }
  onStreamManagement(hook: (event: StreamManagementTelemetry) => void) { this.events.on("streamManagement", hook); }

  private emitError(event: XmppErrorEvent) { this.events.emitSafe("error", event); }

  private emitStatus(snapshot: XmppStatusSnapshot) {
    this.events.emit("status", snapshot);
    const meta = this.reconnect.noteStatus(snapshot);
    this.events.emitSafe("statusHook", snapshot, meta);
  }

  private emitSessionLifecycle(event: SessionLifecycleEvent) {
    this.events.emit("sessionLifecycle", event);
    this.events.emitSafe("sessionLifecycleHook", event);
  }

  private clearReconnectTimer() {
    this.reconnect.clearTimer();
  }

  // C2: after retry exhaustion nothing would ever try again when the
  // network returns. Armed from `onExhausted`; the fresh-budget connect
  // the listener fires disarms it (via `connectInternal`), as does the
  // `disconnect()` logout path.
  private onlineRecoveryListener: (() => void) | null = null;

  private armOnlineRecovery(): void {
    if (typeof window === "undefined" || this.onlineRecoveryListener) return;
    const listener = () => {
      this.disarmOnlineRecovery();
      // The network coming back is a user-environment recovery signal —
      // one of the two fresh-budget entry points (the other being a
      // page load, which constructs a new client).
      void this.connectWithFreshBudget().catch(() => undefined);
    };
    this.onlineRecoveryListener = listener;
    window.addEventListener("online", listener);
  }

  private disarmOnlineRecovery(): void {
    if (typeof window === "undefined" || !this.onlineRecoveryListener) return;
    window.removeEventListener("online", this.onlineRecoveryListener);
    this.onlineRecoveryListener = null;
  }

  private clearResumeState() {
    this.resume.clearAll();
    // Only called from the `destroying` path — i.e. intentional
    // logout / shutdown. Drop the catch-up cursors too so a future
    // login on the same account (same browser) doesn't replay
    // ancient MAM history. (Transient disconnects do NOT go through
    // this method; they intentionally keep cursors so the next
    // resume can fill the gap.)
    this.catchup.reset();
  }

  persistResumeStateForPageHide(): void {
    if (this.disposed) return;
    this.resume.persistForPageHide(
      this.xmpp?.get_resume_state?.() ?? null,
      this.resource,
      () => this.persistRetainedJoinedRooms(),
    );
  }

  /**
   * XMPP owns the page lifecycle so persistence cannot depend on whether a
   * call surface is mounted. This is synchronous by browser design: pagehide
   * cannot await a network round trip.
   */
  prepareForPageHide(): void {
    if (this.disposed) return;
    let acknowledgementUnavailable = false;
    try {
      // `pagehide` must never await I/O. The WASM owner synchronously writes
      // the typed `<r/>` itself (or reports the already-pending request) before
      // returning; persistence still runs for every outcome.
      if (this.xmpp) {
        const outcome = this.xmpp.try_request_stream_management_ack_for_pagehide?.();
        acknowledgementUnavailable = outcome !== PAGEHIDE_SM_ACK_SENT
          && outcome !== PAGEHIDE_SM_ACK_ALREADY_PENDING;
      }
    } catch {
      acknowledgementUnavailable = true;
    } finally {
      this.persistResumeStateForPageHide();
    }
    if (acknowledgementUnavailable) {
      // The page-lifecycle owner maps this local failure to its closed
      // `prepare-xmpp` telemetry operation; no raw queue or stream state leaves
      // the client boundary.
      throw new Error("XMPP pagehide acknowledgement was unavailable");
    }
  }

  /** A BFCache restore retains this client; reconnect only if it lost its wire. */
  resumeAfterPageShow(): void {
    if (!this.disposed && !this.destroying && !this.connected) {
      void this.connect().catch(() => undefined);
    }
  }

  private async enableCarbons(xmpp: XmppClientInstance & { enableCarbons?: () => Promise<void> }) {
    if (xmpp.enableCarbons) {
      try { await xmpp.enableCarbons(); } catch {}
      return;
    }
    if (!xmpp.send_raw_iq) return;
    try { await xmpp.send_raw_iq(`<iq type="set" id="${crypto.randomUUID()}"><enable xmlns="urn:xmpp:carbons:2"/></iq>`); } catch {}
  }

  /** Seam for tests: instance-level indirection over the module-level
   * WASM loader so a stalled load can be simulated. */
  private loadModule: () => Promise<WasmModule> = loadWasmModule;
  /** C3 (#1164 follow-up): generation token for connect attempts. Bumped
   * when a new `doConnect` starts and when the connect timeout tears an
   * attempt down, so a continuation resuming after a stalled `await`
   * (e.g. the WASM module load) can detect it was superseded and abort
   * WITHOUT creating a second live handle that would bind the same
   * resource and trigger a self-inflicted terminal `conflict`. */
  private connectEpoch = 0;

  private async doConnect(): Promise<void> {
    const epoch = ++this.connectEpoch;
    const websocketUrl = this.session.xmpp_websocket_url;
    const mod = await this.loadModule();
    // Stale continuation: a newer connect attempt started (or the
    // timeout tore this one down) while the module load was pending.
    // Abort before creating a handle — the current attempt owns the
    // connection now.
    if (epoch !== this.connectEpoch || this.destroying || this.disposed) return;
    const createConfig = () => new mod.WaddleConfig(
      websocketUrl,
      this.session.jid,
      this.session.session_id,
      this.resource,
    );
    let config = createConfig();
    if (this.resume.handle && typeof config.with_resume_state_handle === "function") {
      config.with_resume_state_handle(this.resume.handle);
      this.clearResumeState();
    } else if (this.resume.state) {
      try {
        applyResumeStateToWasmConfig(config, this.resume.state);
      } catch {
        // The Rust WASM boundary owns XML and RFC3339 parsing. A corrupt
        // persisted SM-only snapshot must not wedge reconnect or leave a
        // partially-mutated config: discard it and use a clean fresh stream.
        this.resume.discardState();
        config = createConfig();
      }
      this.resume.discardState();
    }
    const xmpp = new mod.WaddleClient(config) as unknown as XmppClientInstance;
    this.xmpp = xmpp;
    this.wireEvents(xmpp);
    await xmpp.connect?.();
  }

  private onceConnected: (() => void) | null = null;
  private onceConnectFailed: ((error: Error) => void) | null = null;
  /** Budget for a single connect attempt to reach session-ready. Instance
   * field (not a module constant) so tests can shrink it. */
  private connectTimeoutMs = 15_000;
  /** F3: the pending attempt's budget timer, tracked on the instance so
   * `disconnect()` can cancel it — an orphaned timer surviving a
   * disconnect would later tear down a NEWER connect's half-open handle
   * (spurious "reconnecting" + a stray teardown). */
  private connectTimer: ReturnType<typeof setTimeout> | null = null;

  /**
   * Internal/background connect path. Nearly every call site routes
   * through here indirectly (`requireConnectedXmpp`, room switching,
   * MAM pagers, send fallbacks, the XEP-0319 idle tracker via
   * `setPresence`) — i.e. background triggers that fire constantly in
   * an attended tab. This path therefore deliberately does NOT reset
   * the retry budget (F1: resetting on every mouse move during an
   * outage made exhaustion unreachable and hammered the server at
   * restarted backoff) and does NOT clear a terminal-error
   * classification. While terminal or exhausted it rejects fast
   * without scheduling retries or emitting status, so
   * `requireConnectedXmpp` callers fail the same way they do for any
   * unavailable session and background pollers cause no status churn.
   *
   * Fresh-budget recovery is reserved for genuinely user-explicit
   * intents via `connectWithFreshBudget` — currently the window
   * `online` recovery listener; a page load gets a fresh budget
   * implicitly by constructing a new client.
   */
  async connect(): Promise<void> {
    if (this.disposed) throw new Error("XMPP client is disposed");
    if (this.xmpp && this.connected) return;
    // Join an in-flight attempt BEFORE the terminal/exhausted gate:
    // during the scheduler's FINAL attempt the timer is already null
    // and the attempt counter is at the cap, so `isExhausted()` reads
    // true for the whole (up to `connectTimeoutMs`) window even though
    // the attempt may still succeed. A user action in that window must
    // ride the pending attempt, not fast-reject.
    if (this.connectPromise) return this.connectPromise;
    if (this.inTerminalErrorState || this.reconnect.isExhausted()) {
      throw new Error("XMPP session is not ready");
    }
    // F5: a backoff retry is already armed — leave the schedule intact.
    // Cancelling it for an immediate attempt lets every background
    // trigger burn an attempt on a fast failure (re-entering
    // `handleDisconnected` → `schedule()`), so <1 min of typing during
    // an outage could exhaust the whole budget into a false terminal
    // error. Fast-reject like any unavailable session; only the
    // scheduler itself (`connectFromScheduler`) and user-explicit
    // recovery (`connectWithFreshBudget`) may clear the timer and
    // launch.
    if (this.reconnect.hasPendingRetry()) {
      throw new Error("XMPP session is not ready");
    }
    return this.connectInternal();
  }

  /**
   * User-explicit recovery entry (C2/F1): restores the retry budget —
   * otherwise a post-exhaustion recovery attempt that fails would
   * re-exhaust instantly — and drops any terminal-error classification
   * (#1164). Internal/background callers must use `connect()` instead.
   */
  private connectWithFreshBudget(): Promise<void> {
    if (this.disposed) return Promise.reject(new Error("XMPP client is disposed"));
    this.reconnect.resetAttempts();
    this.inTerminalErrorState = false;
    this.terminalDisconnectDetail = null;
    return this.connectInternal();
  }

  /** Reconnect-scheduler entry point: same connect machinery, but the
   * retry budget is left alone so the attempt cap can exhaust. */
  private connectFromScheduler(): Promise<void> {
    return this.connectInternal();
  }

  private async connectInternal(): Promise<void> {
    if (this.disposed) throw new Error("XMPP client is disposed");
    if (this.xmpp && this.connected) return;
    if (this.connectPromise) return this.connectPromise;
    this.destroying = false;
    this.disarmOnlineRecovery();
    this.clearReconnectTimer();
    this.connectPromise = withSpan(
      "xmpp.connect",
      { "waddle.xmpp.transport": "websocket" },
      () => new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => {
          // C1: the connect budget measures to session-ready, not to
          // catch-up completion. `connect()`'s promise still resolves
          // only after the resume barrier (callers rely on "connected
          // AND caught up"), but once the session is established a
          // slow MAM catch-up must not be treated as a stalled
          // connect — tearing it down would drop the live handle,
          // lose the buffered resume traffic, and livelock
          // structurally-slow catch-ups into terminal error.
          if (this.connected && this.xmpp) return;
          // #1164: a stalled connect must not strand the client in
          // limbo. Tear the half-open handle down (so a late
          // session-ready from it can't land — `isCurrentXmpp` guards
          // key off `this.xmpp`), tell the UI we're still trying, and
          // hand the retry to the backoff scheduler.
          // C3: invalidate any continuation of this attempt still
          // stalled on an `await` (module load) so it can't race a
          // newer attempt into a second live handle.
          this.connectEpoch += 1;
          this.connectTimer = null;
          this.connectPromise = null;
          this.onceConnected = null;
          this.onceConnectFailed = null;
          const stalled = this.xmpp;
          if (stalled) {
            this.xmpp = null;
            void Promise.resolve(stalled.disconnect?.()).catch(() => undefined);
          }
          // An armed terminal detail is stale by construction here: it
          // belongs to the handle we just destroyed, whose disconnect
          // callback (the sole consumer) is now dropped by the handle
          // guard. Left in place it would poison the scheduler's next
          // attempt — its first transient disconnect would read as
          // terminal. The retry re-encounters a genuinely terminal
          // condition and re-classifies it via `set_on_error`.
          this.terminalDisconnectDetail = null;
          if (!this.destroying) {
            this.emitStatus({ state: "reconnecting", detail: "Connection attempt timed out — retrying" });
            this.reconnect.schedule();
          }
          reject(new Error("Reconnection timed out"));
        }, this.connectTimeoutMs);
        this.connectTimer = timeout;
        const done = (fn: () => void) => {
          clearTimeout(timeout);
          if (this.connectTimer === timeout) this.connectTimer = null;
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

  async disconnect(): Promise<void> {
    if (this.disposed) return;
    return this.disconnectInternal();
  }

  private async disconnectInternal(): Promise<void> {
    this.destroying = true;
    this.callTransportRecovery.dispose();
    this.outboundQueue.dispose();
    this.roomDiscoveryGeneration += 1;
    this.currentRoomCatalogFingerprintEvidence.clear();
    this.disarmOnlineRecovery();
    this.clearReconnectTimer();
    // F3: cancel the pending connect attempt outright. Bump the epoch
    // so a continuation stalled on the module load aborts, clear the
    // budget timer so it cannot fire later against a NEWER connect's
    // half-open handle, and settle the pending `connect()` promise so
    // its awaiter isn't left dangling (or spuriously rejected by the
    // orphaned timer long after this disconnect).
    this.connectEpoch += 1;
    if (this.connectTimer) {
      clearTimeout(this.connectTimer);
      this.connectTimer = null;
    }
    const failPendingConnect = this.onceConnectFailed;
    this.onceConnected = null;
    this.onceConnectFailed = null;
    failPendingConnect?.(new Error("XMPP disconnected"));
    // A disconnect is user-explicit (logout): the terminal-error latch
    // must not survive into a later session on this instance. The armed
    // detail moves with it — the disconnect callback that would consume
    // it is dropped by the handle guard once we tear down here, and a
    // leftover detail would make the NEXT session's first transient
    // disconnect flip to a false terminal "sign in again".
    this.inTerminalErrorState = false;
    this.terminalDisconnectDetail = null;
    this.clearResumeState();
    // A user-explicit teardown drops the carried resume buffer too — a
    // later session on this instance must not replay another session's
    // stanzas.
    this.carriedPendingDuringResume = [];
    const xmpp = this.xmpp;
    const joinedRooms = [...this.joinedMucs.keys()];
    this.stopSelfPing();
    this.connected = false;
    this.connectPromise = null;
    this.currentRoom = null;
    this.roomSwitchPromise = null;
    this.roomSwitchTarget = null;
    this.roomJoinRetry.cancelAll();
    this.rejectRoomJoinWaiters(new Error("XMPP disconnected while joining a room"));
    this.uploadServiceJid = null;
    this.clearRoomPresenceCaches();
    this.retainedJoinedRoomJids.clear();
    this.joinedMucs.clear();
    this.joinedMucJoinTokens.clear();
    this.joinedMucReady.clear();
    this.autoJoinAttemptedRoomKeys.clear();
    this.resumedSessionRoomKeys.clear();
    this.sessionReadyHandledXmpp = null;
    clearDmCallJoinCacheForAccount(this.session.jid);
    clearMucCallSessionCacheForAccount(this.session.jid);
    clearDmCallActivities();
    clearMucCallParticipants();
    clearAllRaisedHands();
    clearAllMuted();
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

  /**
   * Terminal client teardown for logout, replacement, and provider unmount.
   * `disconnect()` deliberately remains reusable for callers that reconnect
   * the same BrowserXmppClient; only a disposed owner releases its durable
   * lease heartbeat.
   */
  async dispose(): Promise<void> {
    if (this.disposePromise) return this.disposePromise;
    this.disposed = true;
    // Release the durable owner synchronously. Transport teardown below can
    // wait indefinitely on a stalled call, room leave, or socket close; it
    // must never keep this terminal owner's lease or resumable tail alive.
    this.clearResumeState();
    this.resume.dispose();
    this.disposePromise = this.disconnectInternal();
    return this.disposePromise;
  }

  private roomJidForChannel(channelId: string): string {
    return this.discoveredRoomJids.get(channelId) ?? roomBareJidFor(this.session, channelId);
  }

  rememberRoomJidForChannel(channelId: string, roomJid: string): void {
    const normalizedRoomJid = barePeerJid(roomJid);
    if (!channelId || !normalizedRoomJid) return;
    this.discoveredRoomJids.set(channelId, normalizedRoomJid);
  }

  private scheduleRoomJoinRetry(roomJid: string): void {
    const key = this.roomJoinKey(roomJid);
    const xmpp = this.xmpp;
    if (!key || !xmpp || this.roomJoinRetry.pending(key)) return;

    const focused = this.currentRoom !== null
      && this.roomJoinKey(this.currentRoom) === key;
    const retained = this.retainedJoinedRoomJids.has(key);
    const autoJoin = this.autoJoinRoomJids.some(
      (candidate) => this.roomJoinKey(candidate) === key,
    );
    if (!focused && !retained && !autoJoin) return;
    const source = focused ? "focused" : retained ? "retained" : "autojoin";

    void this.roomJoinRetry.schedule(
      key,
      this.roomJoinRetryOptions(roomJid, key, xmpp, source),
    ).catch(() => undefined);
  }

  private roomJoinRetryOptions(
    roomJid: string,
    key: string,
    xmpp: XmppClientInstance,
    source: "focused" | "retained" | "autojoin",
  ): ScheduleRoomJoinRetryOptions {
    return {
      // A focused retry belongs to that navigation intent and stops as soon
      // as focus moves elsewhere. A background retry likewise belongs to the
      // retained/autojoin source that admitted it; another source appearing
      // later must not silently extend the retry's lifetime.
      isEligible: () => this.isCurrentXmpp(xmpp) && (
        source === "focused"
          ? this.currentRoom !== null && this.roomJoinKey(this.currentRoom) === key
          : source === "retained"
            ? this.retainedJoinedRoomJids.has(key)
            : this.autoJoinRoomJids.some((candidate) => this.roomJoinKey(candidate) === key)
      ),
      retry: () => this.ensureJoined(roomJid),
    };
  }

  private waitForRoomSelfPresence(roomJid: string, requestedNick: string): Promise<void> {
    const key = this.roomJoinKey(roomJid);
    // Concurrent joins for JIDs that normalize to the same room key
    // (e.g. case variants from topology vs. retained-room replay) must
    // share one waiter — a second `set` would orphan the first promise,
    // hanging that join until its timeout.
    const existing = this.roomJoinWaiters.get(key);
    if (existing) return existing.promise;
    let resolveJoin!: () => void;
    let rejectJoin!: (error: Error) => void;
    const promise = new Promise<void>((resolve, reject) => {
      resolveJoin = resolve;
      rejectJoin = reject;
    });
    const timeout = setTimeout(() => {
      this.roomJoinWaiters.delete(key);
      const detail = `Timed out waiting for self-presence in ${roomJid}`;
      const rejection = new Error(
        "Channel presence did not finish syncing. Try again in a moment.",
      );
      this.scheduleRoomJoinRetry(roomJid);
      this.emitError({
        kind: "muc-join-timeout",
        recoverable: true,
        detail,
        cause: rejection,
      });
      rejectJoin(rejection);
    }, ROOM_SELF_PRESENCE_TIMEOUT_MS);
    this.roomJoinWaiters.set(key, {
      promise,
      requestedNick,
      resolve: () => {
        clearTimeout(timeout);
        this.roomJoinWaiters.delete(key);
        resolveJoin();
      },
      reject: (error: Error) => {
        clearTimeout(timeout);
        this.roomJoinWaiters.delete(key);
        rejectJoin(error);
      },
    });
    return promise;
  }

  private clearRoomPresenceCaches(): void {
    this.presence.clearAll();
  }

  private memberJidsFor(roomJid: string): Record<string, string> {
    return this.presence.memberJidsFor(roomJid);
  }

  private dispatchFocusedRoomHandlers(): void {
    this.presence.dispatchFocusedRoom();
  }

  async ensureJoined(roomJid: string): Promise<void> {
    // Every join tracker keys on the canonical `roomJoinKey` (lowercased
    // bare JID) so case variants from topology vs. retained-room replay
    // resolve to a single entry (#1221). The raw `roomJid` still goes on
    // the wire (`performMucJoin`, `rememberJoinedRoom`).
    const key = this.roomJoinKey(roomJid);
    if (this.joinedMucReady.has(key)) return;
    const scheduledRetry = this.roomJoinRetry.pending(key);
    if (scheduledRetry) return scheduledRetry;
    const existing = this.joinedMucs.get(key);
    if (existing) {
      const xmpp = this.xmpp;
      if (
        xmpp
        && this.currentRoom !== null
        && this.roomJoinKey(this.currentRoom) === key
      ) {
        // A quick return adopts the still-running wire attempt. If it fails,
        // the new focused intent may schedule another backoff window without
        // sending a duplicate join presence in the meantime.
        this.roomJoinRetry.reactivate(
          key,
          this.roomJoinRetryOptions(roomJid, key, xmpp, "focused"),
        );
      }
      const existingToken = this.joinedMucJoinTokens.get(key);
      await existing;
      if (existingToken && this.joinedMucJoinTokens.get(key) !== existingToken) {
        return;
      }
      if (!existingToken && !this.joinedMucs.has(key)) return;
      this.joinedMucReady.add(key);
      this.clearAutoJoinBlock(key);
      this.rememberJoinedRoom(roomJid);
      return;
    }
    const promise = this.performMucJoin(roomJid);
    const joinToken = Symbol(key);
    this.joinedMucs.set(key, promise);
    this.joinedMucJoinTokens.set(key, joinToken);
    try {
      await promise;
      if (this.joinedMucJoinTokens.get(key) === joinToken) {
        this.roomJoinRetry.complete(key);
        this.joinedMucReady.add(key);
        this.clearAutoJoinBlock(key);
        this.rememberJoinedRoom(roomJid);
      }
    } catch (err) {
      if (this.joinedMucJoinTokens.get(key) === joinToken) {
        this.joinedMucs.delete(key);
        this.joinedMucJoinTokens.delete(key);
        this.joinedMucReady.delete(key);
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

  private persistAutoJoinBlocks(): void {
    this.resumePersistence.saveAutoJoinBlocks?.([...this.terminallyDeniedAutoJoinRooms.values()]);
  }

  private clearAutoJoinBlock(roomJid: string): void {
    const key = this.roomJoinKey(roomJid);
    if (!key || !this.terminallyDeniedAutoJoinRooms.delete(key)) return;
    this.autoJoinAttemptedRoomKeys.delete(key);
    this.persistAutoJoinBlocks();
    this.events.emit("roomAccessChanged", {
      roomJid: key,
      state: "available",
    });
  }

  private async performMucJoin(roomJid: string): Promise<void> {
    const xmpp = this.xmpp;
    if (!xmpp) throw new Error("XMPP session is not ready");
    if (!xmpp.join_room && !xmpp.joinRoom) {
      throw new Error("XMPP client missing join_room binding");
    }
    const ready = this.waitForRoomSelfPresence(roomJid, this.session.username);
    const joinKey = this.roomJoinKey(roomJid);
    const joinWaiter = this.roomJoinWaiters.get(joinKey);
    try {
      if (xmpp.join_room) {
        await xmpp.join_room(roomJid, this.session.username);
      } else if (xmpp.joinRoom) {
        await xmpp.joinRoom(roomJid, this.session.username);
      }
    } catch (error) {
      const joinError = error instanceof Error
        ? error
        : new Error("XMPP room join failed before presence could sync");
      if (this.isCurrentXmpp(xmpp) && this.roomJoinWaiters.get(joinKey) === joinWaiter) {
        joinWaiter?.reject(joinError);
      }
      await ready.catch(() => undefined);
      throw joinError;
    }
    if (!this.isCurrentXmpp(xmpp)) {
      await ready.catch(() => undefined);
      return;
    }
    await ready;
  }

  async fanOutAutoJoin(roomJids: ReadonlyArray<string>, concurrency = 6): Promise<void> {
    if (!this.xmpp) return;
    // Dedup the input by canonical key and skip rooms already attempted
    // this epoch; the raw JID enters the queue for the wire (#1221).
    const queue: string[] = [];
    const seenThisCall = new Set<string>();
    for (const roomJid of roomJids) {
      if (!roomJid) continue;
      const key = this.roomJoinKey(roomJid);
      if (!key || seenThisCall.has(key)) continue;
      seenThisCall.add(key);
      if (this.roomIsTerminallyDenied(roomJid)) continue;
      if (this.autoJoinAttemptedRoomKeys.has(key)) continue;
      this.autoJoinAttemptedRoomKeys.add(key);
      queue.push(roomJid);
    }
    const workers = Array.from({ length: Math.max(1, concurrency) }, async () => {
      while (queue.length > 0) {
        const roomJid = queue.shift();
        if (!roomJid) continue;
        try {
          await this.ensureJoined(roomJid);
        } catch {
          // Failed joins drop their tracker entry inside ensureJoined,
          // but the key stays in `autoJoinAttemptedRoomKeys`, so a
          // same-epoch fan-out (e.g. a topology refresh) will NOT retry
          // (#1221). Retry happens on a reconnect (new epoch clears the
          // set) or when the user navigates to the room (`switchRoom` →
          // `ensureJoined`, which is not single-flight-gated).
        }
      }
    });
    await Promise.allSettled(workers);
  }

  async retryRoomAccess(spaceId: string, channelId: string): Promise<void> {
    const roomJid = this.roomJidForChannel(channelId);
    if (!this.roomIsTerminallyDenied(roomJid)) return;
    await this.switchRoomForIntent(spaceId, channelId, "explicit-navigation");
  }

  async switchRoom(spaceId: string, channelId: string) {
    await this.switchRoomForIntent(spaceId, channelId, "automatic");
  }

  private async switchRoomForIntent(
    _spaceId: string,
    channelId: string,
    intent: "automatic" | "explicit-navigation",
  ) {
    await this.connect();
    const nextRoom = this.roomJidForChannel(channelId);
    if (
      intent !== "explicit-navigation"
      && this.roomIsTerminallyDenied(nextRoom)
    ) {
      this.currentRoom = nextRoom;
      this.dispatchFocusedRoomHandlers();
      throw new Error("You need access to this channel.");
    }
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
    if (this.currentRoom && this.roomJoinKey(this.currentRoom) !== this.roomJoinKey(nextRoom)) {
      this.roomJoinRetry.cancel(this.roomJoinKey(this.currentRoom));
    }
    this.currentRoom = nextRoom;
    this.dispatchFocusedRoomHandlers();
    try {
      await withSpan(
        "xmpp.room_switch",
        { "conversation.kind": "room" },
        () => this.ensureJoined(nextRoom),
      );
    } catch (err) {
      if (!this.isCurrentXmpp(xmpp)) return;
      throw err;
    }
    if (!this.isCurrentXmpp(xmpp)) return;
    this.dispatchFocusedRoomHandlers();
    this.startSelfPing();
    await this.flushQueuedRoomMessages(nextRoom);
  }

  private canUseConnectedSession(): boolean {
    return !!this.xmpp && this.connected && !this.destroying && !this.disposed && !browserOffline();
  }

  private roomIsReady(roomJid: string): boolean {
    return this.canUseConnectedSession() && this.currentRoom === roomJid && this.joinedMucReady.has(this.roomJoinKey(roomJid));
  }

  private enqueueReason(): string {
    if (browserOffline()) return "offline";
    if (this.disposed) return "disposed";
    if (this.destroying) return "destroying";
    if (!this.xmpp) return "no-client";
    if (!this.connected) return "reconnecting";
    return "not-ready";
  }

  private async compatSendGroupMessage(xmpp: XmppClientInstance, roomJid: string, body: string, opts: SendGroupMessageOptions): Promise<string | null> {
    const { effectiveBody, replyFallbackLength, rebasedMarkup, rebasedReferences } = encodeBodyForSend(body, opts.replyTo, opts.markup, opts.references);
    const wasmOpts = buildWasmSendOptions({ ...opts, markup: rebasedMarkup, references: rebasedReferences, requestDisplayedMarker: opts.requestDisplayedMarker ?? true }, replyFallbackLength);
    if (xmpp.send_groupchat_message) {
      return compatWasmSendResult(await xmpp.send_groupchat_message(roomJid, effectiveBody, wasmOpts) as string | WasmSendMessageOutcome);
    }
    throw new Error("XMPP session is not ready");
  }

  private async compatSendDirectMessage(xmpp: XmppClientInstance, peerJid: string, body: string, opts: SendDirectMessageOptions): Promise<string | null> {
    const { effectiveBody, replyFallbackLength, rebasedMarkup, rebasedReferences } = encodeBodyForSend(body, opts.replyTo, opts.markup, opts.references);
    const wasmOpts = buildWasmSendOptions({ ...opts, markup: rebasedMarkup, references: rebasedReferences, requestDisplayedMarker: opts.requestDisplayedMarker ?? true }, replyFallbackLength);
    if (xmpp.send_chat_message) {
      return compatWasmSendResult(await xmpp.send_chat_message(peerJid, effectiveBody, wasmOpts) as string | WasmSendMessageOutcome);
    }
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

  private async compatSendInCallReaction(xmpp: XmppClientInstance, to: string, type: "chat" | "groupchat", sid: string, emoji: string) {
    if (xmpp.send_in_call_reaction) return xmpp.send_in_call_reaction(to, type, sid, emoji);
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
    if (!this.xmpp || !this.connected || this.destroying || this.disposed || this.currentRoom !== roomJid) throw new Error(`Room is not ready: ${roomJid}`);
    return { xmpp: this.xmpp, roomJid };
  }

  /** Send a queued DM through the current session (OfflineSendQueue drain callback). */
  private sendQueuedDirectMessage(peerJid: string, body: string, opts: SendDirectMessageOptions & { id: string }): Promise<string | null> {
    if (this.disposed) throw new Error("XMPP client is disposed");
    const xmpp = this.xmpp;
    if (!xmpp) throw new Error("XMPP session is not ready");
    return this.compatSendDirectMessage(xmpp, peerJid, body, opts);
  }

  /** Send a queued room message through the current session (OfflineSendQueue drain callback). */
  private sendQueuedRoomMessage(roomJid: string, body: string, opts: SendGroupMessageOptions & { id: string }): Promise<string | null> {
    if (this.disposed) throw new Error("XMPP client is disposed");
    const xmpp = this.xmpp;
    if (!xmpp) throw new Error("XMPP session is not ready");
    return this.compatSendGroupMessage(xmpp, roomJid, body, opts);
  }

  private flushQueuedDirectMessages(): Promise<void | undefined> {
    return this.outboundQueue.flushDirect();
  }

  private flushQueuedRoomMessages(roomJid: string): Promise<void | undefined> {
    return this.outboundQueue.flushRoom(roomJid);
  }

  async sendGroupMessage(spaceId: string, channelId: string, body: string, opts: SendGroupMessageOptions = {}): Promise<OutboundSendResult | null> {
    if (this.disposed) throw new Error("XMPP client is disposed");
    const hasFiles = !!opts.files?.length;
    const hasThreadMetadata = !!opts.threadId?.trim();
    const hasForumMetadata = !!opts.threadCreate?.title?.trim() || !!opts.threadReply?.threadId?.trim();
    if (!body.trim() && !hasFiles && !hasThreadMetadata && !hasForumMetadata) return null;
    const roomJid = this.roomJidForChannel(channelId);
    if (this.roomIsReady(roomJid) && this.xmpp) {
      const outboundId = opts.id ?? crypto.randomUUID();
      const sendOpts = { ...opts, id: outboundId, mentionJidsByNick: { ...(opts.mentionJidsByNick ?? {}), ...this.memberJidsFor(roomJid) } };
      this.outboundQueue.persistPendingRoomSend(roomJid, body, sendOpts);
      // Claim before the compat host call. A host may synchronously deliver
      // the matching XEP-0198 ack, in which case marking afterwards would
      // resurrect an already-settled in-flight claim.
      const attempt = this.outboundQueue.beginLiveAttempt(outboundId, "room");
      let id: string | null;
      try {
        id = await this.compatSendGroupMessage(this.xmpp, roomJid, body, sendOpts);
      } catch (error) {
        this.outboundQueue.rollbackLiveAttempt(attempt);
        if (isNonRetryableWasmSendFailure(error)) this.outboundQueue.discardNonRetryable(attempt);
        throw error;
      }
      if (id !== outboundId) this.outboundQueue.rollbackLiveAttempt(attempt);
      return { id, state: "sending" };
    }
    const queued = this.outboundQueue.queueRoomMessage(roomJid, body, opts);
    void this.connect().then(() => this.switchRoom(spaceId, channelId)).then(() => this.flushQueuedRoomMessages(roomJid)).catch(() => undefined);
    return queued;
  }

  async lookupLinkPreview(body: string, scopeJid: string): Promise<LinkPreviewLookupResult | null> {
    return requestPlaintextLinkPreviewLookup(
      this.xmpp,
      body,
      scopeJid,
      this.trustedLinkPreviewMediaOrigin(),
    );
  }

  /** XEP-0045 §7.5 (#1256): a MUC PM conversation is keyed by the full
   * occupant JID and replies MUST go to `room@service/nick` — sending to
   * the room bare JID would broadcast. Everything else addresses the
   * bare peer JID. */
  private directMessageAddress(peerJid: string, scope?: DmConversationScope): string {
    const mucPm = scope ? scope === "muc-occupant" : this.isMucPmPeer(peerJid);
    return mucPm ? peerJid : barePeerJid(peerJid);
  }

  async sendDirectMessage(peerJid: string, body: string, opts: SendDirectMessageOptions = {}): Promise<OutboundSendResult | null> {
    if (this.disposed) throw new Error("XMPP client is disposed");
    if (!body.trim() && !opts.files?.length) return null;
    const explicitScope = typeof opts.mucPm === "boolean"
      ? opts.mucPm ? "muc-occupant" : "account"
      : undefined;
    const normalizedPeerJid = this.directMessageAddress(peerJid, explicitScope);
    // XEP-0045 §7.5: mark MUC PMs so the builder appends the muc#user
    // <x/> element (sent-carbon classification on our other devices).
    const mucPm = normalizedPeerJid.includes("/");
    if (this.canUseConnectedSession() && this.xmpp) {
      const outboundId = opts.id ?? crypto.randomUUID();
      const sendOpts = { ...opts, id: outboundId, ...(mucPm ? { mucPm: true } : {}) };
      this.outboundQueue.persistPendingDirectSend(normalizedPeerJid, body, sendOpts);
      // See the room-send path: the durable claim must precede a host that
      // can synchronously dispatch its matching SM acknowledgement.
      const attempt = this.outboundQueue.beginLiveAttempt(outboundId, "dm");
      let id: string | null;
      try {
        id = await this.compatSendDirectMessage(this.xmpp, normalizedPeerJid, body, sendOpts);
      } catch (error) {
        this.outboundQueue.rollbackLiveAttempt(attempt);
        if (isNonRetryableWasmSendFailure(error)) this.outboundQueue.discardNonRetryable(attempt);
        throw error;
      }
      if (id !== outboundId) this.outboundQueue.rollbackLiveAttempt(attempt);
      return { id, state: "sending" };
    }
    return this.outboundQueue.queueDirectMessage(normalizedPeerJid, body, mucPm ? { ...opts, mucPm: true } : opts);
  }

  async sendChatState(spaceId: string, channelId: string, state: ChatStateType, thread?: { id: string; parent?: string }) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendChatState(xmpp, roomJid, "groupchat", state, thread); }
  async sendDisplayed(spaceId: string, channelId: string, messageId: string, thread?: { id: string; parent?: string }) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendDisplayed(xmpp, roomJid, "groupchat", messageId, thread); }
  async sendReaction(spaceId: string, channelId: string, messageId: string, emojis: string[], thread?: { id: string; parent?: string }) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendReaction(xmpp, roomJid, "groupchat", messageId, emojis, thread); }
  async sendInCallReaction(spaceId: string, channelId: string, sid: string, emoji: string) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendInCallReaction(xmpp, roomJid, "groupchat", sid, emoji); }
  /**
   * #1029: raise or lower this client's hand in the active MUC call. The
   * raised-hand state is a presence extension carried alongside `<muji/>`,
   * so this re-emits the active call presence with the flag toggled. The
   * call-action reads the active call's room/nick/video from `$callState`;
   * this wrapper only supplies the raw wasm sender. Returns whether the
   * presence was emitted (no-op outside an active MUC call).
   */
  async setCallHandRaised(raised: boolean): Promise<boolean> {
    const xmpp = await this.requireConnectedXmpp();
    return setMucCallHandRaised(xmpp as unknown as RawIqSender, raised);
  }
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
  async pinDirectMessage(peerJid: string, targetStanzaId: string) {
    const xmpp = await this.requireConnectedXmpp();
    const normalizedPeerJid = barePeerJid(peerJid);
    if (typeof xmpp.pin_direct_message !== "function") throw new Error("pin_direct_message not available in wasm client");
    await xmpp.pin_direct_message(normalizedPeerJid, targetStanzaId);
  }
  async unpinDirectMessage(peerJid: string, targetStanzaId: string) {
    const xmpp = await this.requireConnectedXmpp();
    const normalizedPeerJid = barePeerJid(peerJid);
    if (typeof xmpp.unpin_direct_message !== "function") throw new Error("unpin_direct_message not available in wasm client");
    await xmpp.unpin_direct_message(normalizedPeerJid, targetStanzaId);
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
  async fetchDirectPins(peerJid: string): Promise<import("./wasm-types").WasmPinEntry[]> {
    const xmpp = await this.requireConnectedXmpp();
    const normalizedPeerJid = barePeerJid(peerJid);
    if (typeof xmpp.fetch_room_pins !== "function") throw new Error("fetch_room_pins not available in wasm client");
    const result = await xmpp.fetch_room_pins(normalizedPeerJid);
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
  async fetchDirectMessagesByStanzaIds(
    peerJid: string,
    stanzaIds: string[],
  ): Promise<import("./wasm-types").WasmArchivedMessage[]> {
    if (stanzaIds.length === 0) return [];
    const xmpp = await this.requireConnectedXmpp();
    const normalizedPeerJid = barePeerJid(peerJid);
    if (typeof xmpp.fetch_direct_messages_by_stanza_ids !== "function") throw new Error("fetch_direct_messages_by_stanza_ids not available in wasm client");
    const page = await xmpp.fetch_direct_messages_by_stanza_ids(normalizedPeerJid, stanzaIds);
    return Array.isArray(page?.messages) ? page.messages : [];
  }
  async sendRetraction(spaceId: string, channelId: string, retractsId: string, thread?: { id: string; parent?: string }) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendRetraction(xmpp, roomJid, "groupchat", retractsId, thread); }
  async sendModeration(spaceId: string, channelId: string, targetId: string, reason?: string) { const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId); await this.compatSendModeration(xmpp, roomJid, targetId, reason); }
  // Corrections (XEP-0308) flow through compatSendCorrection's options dict,
  // which already serializes thread/parent into the wasm send options used by
  // build_outbound_message — so passing { threadId, parentThreadId } here is
  // all the wire-level conformance needs.
  async sendCorrection(spaceId: string, channelId: string, body: string, replacesId: string, markup?: SendGroupMessageOptions["markup"], references?: SendGroupMessageOptions["references"], thread?: { id: string; parent?: string }, preview?: Pick<SendGroupMessageOptions, "linkPreviewToken" | "linkPreviewExpiresAt">): Promise<string | null> {
    const { xmpp, roomJid } = await this.requireJoinedRoom(spaceId, channelId);
    const opts: SendGroupMessageOptions = { markup, references, ...preview };
    if (thread?.id) opts.threadId = thread.id;
    if (thread?.parent) opts.parentThreadId = thread.parent;
    return await this.compatSendCorrection(xmpp, roomJid, "groupchat", body, replacesId, opts);
  }
  async sendDmChatState(peerJid: string, state: ChatStateType, thread?: { id: string; parent?: string }, scope?: DmConversationScope): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendChatState(xmpp, this.directMessageAddress(peerJid, scope), "chat", state, thread); }
  async sendDmDisplayed(peerJid: string, messageId: string, thread?: { id: string; parent?: string }, scope?: DmConversationScope): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendDisplayed(xmpp, this.directMessageAddress(peerJid, scope), "chat", messageId, thread); }
  async sendDmRetraction(peerJid: string, messageId: string, thread?: { id: string; parent?: string }, scope?: DmConversationScope): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendRetraction(xmpp, this.directMessageAddress(peerJid, scope), "chat", messageId, thread); }
  async sendDmCorrection(peerJid: string, body: string, replacesId: string, markup?: SendDirectMessageOptions["markup"], references?: SendDirectMessageOptions["references"], preview?: Pick<SendDirectMessageOptions, "linkPreviewToken" | "linkPreviewExpiresAt">, thread?: { id: string; parent?: string }, scope?: DmConversationScope): Promise<string | null> {
    const xmpp = await this.requireConnectedXmpp();
    const address = this.directMessageAddress(peerJid, scope);
    const opts: SendDirectMessageOptions = { markup, references, ...preview, ...(address.includes("/") ? { mucPm: true } : {}) };
    if (thread?.id) opts.threadId = thread.id;
    if (thread?.parent) opts.parentThreadId = thread.parent;
    return await this.compatSendCorrection(xmpp, address, "chat", body, replacesId, opts);
  }
  async sendDmReaction(peerJid: string, messageId: string, emojis: string[], thread?: { id: string; parent?: string }, scope?: DmConversationScope): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendReaction(xmpp, this.directMessageAddress(peerJid, scope), "chat", messageId, emojis, thread); }
  async sendDmInCallReaction(peerJid: string, sid: string, emoji: string): Promise<void> { const xmpp = await this.requireConnectedXmpp(); await this.compatSendInCallReaction(xmpp, peerJid, "chat", sid, emoji); }

  /**
   * XEP-0490 §3 multi-device "read up to here" publish. `chatId` is
   * bare for a DM contact or room, and the full occupant JID for a MUC PM;
   * `stanzaId` is
   * the XEP-0359 id of the latest displayed message; `stanzaIdBy` is
   * the JID that injected that stanza-id (room for MUC, user's own
   * server for 1:1). Failures are intentionally silent — MDS is a
   * best-effort multi-device-sync signal, not a UX-visible action.
   */
  async publishMdsDisplayed(chatId: string, stanzaId: string, stanzaIdBy: string): Promise<void> {
    const xmpp = await this.requireConnectedXmpp();
    if (typeof xmpp.publish_mds_displayed !== "function") return;
    if (!(await this.supportsMdsPublishOptions(xmpp))) return;
    try { await xmpp.publish_mds_displayed(chatId, stanzaId, stanzaIdBy); } catch { /* best-effort */ }
  }

  private supportsMdsPublishOptions(xmpp: XmppClientInstance): Promise<boolean> {
    if (typeof xmpp.supports_mds_publish_options !== "function") return Promise.resolve(false);
    this.mdsPublishOptionsSupport ??= xmpp
      .supports_mds_publish_options()
      .then(Boolean)
      .catch(() => false);
    return this.mdsPublishOptionsSupport;
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

  setMdsDisplayedHandler(handler: ((entry: MdsDisplayedEntry) => void) | null) { this.events.set("mdsDisplayed", handler); }

  async subscribeStoryReactionSummaries(communityJid: string): Promise<void> { return this.pubsub.subscribeStoryReactionSummaries(communityJid); }

  async subscribeStories(communityJid: string): Promise<void> { return this.pubsub.subscribeStories(communityJid); }

  addPubsubEventHandler(handler: (event: PubsubEvent) => void): () => void {
    return this.events.on("pubsubEvent", handler);
  }

  setPubsubEventHandler(handler: ((event: PubsubEvent) => void) | null) {
    this.events.set("pubsubEvent", handler);
  }

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
      results.push({ url: result.getUrl, name: prepared.originalName, mediaType: prepared.originalMediaType, size: prepared.originalSize, disposition: inferredFileDisposition(prepared.originalMediaType, prepared.originalName), hashes: prepared.plaintextHashes, encrypted: { ...prepared.encrypted, sources: [result.getUrl] } });
    }
    return results;
  }

  async invokeExtensionLaunch(launch: ExtensionLaunchDescriptor): Promise<ExtensionCommandResult> { const xmpp = await this.requireConnectedXmpp(); return invokeExtensionLaunch(xmpp as WasmClient, this.session.jid, launch); }
  async discoverExtensionCommands(): Promise<DiscoveredExtensionCommand[]> { const xmpp = await this.requireConnectedXmpp(); return discoverExtensionCommands(xmpp as WasmClient, this.session.jid); }
  async discoverExtensionRoutes(): Promise<DiscoveredExtensionRoute[]> { const xmpp = await this.requireConnectedXmpp(); return discoverExtensionRoutes(xmpp as WasmClient, this.session.jid); }
  async fetchExtensionRouteItems(route: DiscoveredExtensionRoute, roomJid: string): Promise<ExtensionRouteItem[]> { const xmpp = await this.requireConnectedXmpp(); return fetchExtensionRouteItems(xmpp as WasmClient, route, roomJid); }
  async invokeExtensionCommand(command: DiscoveredExtensionCommand, roomJid?: string): Promise<ExtensionCommandResult> { const xmpp = await this.requireConnectedXmpp(); return invokeExtensionCommand(xmpp as WasmClient, this.session.jid, command, roomJid); }
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
   * Fetch the XEP-0215 (`urn:xmpp:extdisco:2`) external services — TURN/STUN —
   * the user's own server advertises, so a call can inject them into LiveKit's
   * connection `rtcConfig.iceServers` (an XMPP-native, client-controlled ICE
   * path).
   *
   * Returns `[]` on EVERY failure mode — never throws — because the caller's
   * fallback is identical in all of them: connect without an `rtcConfig` and
   * let LiveKit use its own signalling-provided ICE servers. Each path emits a
   * precise `console.warn` for diagnostics:
   *   - session not ready (mid-reconnect / mid-teardown race)
   *   - the wasm `fetch_external_services` method is missing (older bundle)
   *   - the IQ times out after 10s
   *   - the IQ is rejected (transport / stanza error)
   *   - the JS-boundary value is not an array (regression on the Rust side)
   *
   * Empty is also the correct success value when the server advertises no
   * services — the caller must not connect with an empty ICE list (that would
   * REPLACE LiveKit's servers with nothing), so `[]` deliberately collapses
   * "none advertised" and "fetch failed" into the same fall-back action.
   */
  async fetchExternalServices(): Promise<ExternalService[]> {
    // Bounded timeout (10s): the wasm-side `send_iq_command` is an unbounded
    // oneshot, and this fetch sits on the call-connect critical path — a
    // stalled server handler must not hang the join.
    const TIMEOUT_MS = 10_000;
    let timeoutHandle: ReturnType<typeof setTimeout> | null = null;
    let timedOut = false;
    const timeoutSentinel = Symbol("extdisco-fetch-timeout");
    const timeoutPromise = new Promise<typeof timeoutSentinel>((resolve) => {
      timeoutHandle = setTimeout(() => {
        timedOut = true;
        resolve(timeoutSentinel);
      }, TIMEOUT_MS);
    });
    try {
      const xmpp = await this.requireConnectedXmpp();
      if (!xmpp.fetch_external_services) {
        console.warn(
          "[xmpp] fetch_external_services is unavailable (older wasm bundle?); " +
          "calls will use LiveKit's default ICE servers",
        );
        return [];
      }
      const fetchPromise = xmpp.fetch_external_services();
      // If the timeout wins the race, the IQ promise settles unobserved later.
      // Attach a no-op catch so a late rejection never surfaces as an
      // unhandledrejection (the race below still handles a rejection that wins).
      fetchPromise.catch(() => {});
      const result = await Promise.race([fetchPromise, timeoutPromise]);
      if (result === timeoutSentinel) {
        console.warn(
          `[xmpp] fetch_external_services timed out after ${TIMEOUT_MS}ms; ` +
          "connecting with LiveKit's default ICE servers.",
        );
        return [];
      }
      if (!Array.isArray(result)) {
        // Bundle drift: the wasm method's signature is `Promise<any>`, so a
        // Rust-side shape regression would otherwise fall through silently.
        // Surface it (mirrors `fetchVapidPublicKey`'s shape hard-check).
        console.warn(
          "[xmpp] fetch_external_services returned a non-array value; " +
          "connecting with LiveKit's default ICE servers:",
          result,
        );
        return [];
      }
      return coerceExternalServices(result);
    } catch (error) {
      if (timedOut) return [];
      console.warn("[xmpp] fetch_external_services IQ rejected:", error);
      return [];
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
      const rejection = parseRegisterPushDeviceRejection(error);
      if (rejection?.code === "session-expired") {
        throw error;
      }
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
  async fetchUserBookmarks(): Promise<UserBookmarkItem[]> { return this.pubsub.fetchUserBookmarks(); }

  async setRoomNotificationMode(opts: {
    roomJid: string;
    mode: "always" | "on-mention" | "never";
    name?: string;
    richPayloadOptIn?: boolean;
  }): Promise<SetRoomNotificationModeOutcome> { return this.pubsub.setRoomNotificationMode(opts); }

  async fetchDmBookmarks(): Promise<DmBookmarkItem[]> { return this.pubsub.fetchDmBookmarks(); }

  async setDmNotificationMode(opts: {
    dmJid: string;
    mode: "always" | "on-mention" | "never";
    richPayloadOptIn: boolean;
  }): Promise<SetDmNotificationModeResult> { return this.pubsub.setDmNotificationMode(opts); }

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
  async fetchThreads(opts: {
    pageSize?: number;
    afterCursor?: string;
    status?: ThreadsStatusFilter;
    activeSince?: string;
    channel?: string;
    search?: string;
    sort?: ThreadsSort;
  } = {}): Promise<WasmThreadsPage> {
    const xmpp = await this.requireConnectedXmpp();
    const payload: WasmFetchThreadsOptions = {
      ...(typeof opts.pageSize === "number" ? { page_size: opts.pageSize } : {}),
      ...(opts.afterCursor ? { after_cursor: opts.afterCursor } : {}),
      ...(opts.status ? { status: opts.status } : {}),
      ...(opts.activeSince ? { active_since: opts.activeSince } : {}),
      ...(opts.channel ? { channel: opts.channel } : {}),
      ...(opts.search ? { search: opts.search } : {}),
      ...(opts.sort ? { sort: opts.sort } : {}),
    };
    const raw = await xmpp.fetch_threads?.(payload) as WasmThreadsPage | undefined;
    return raw ?? { total: 0, unread_threads: 0, entries: [] };
  }

  async fetchFeed(communityJid: string, maxItems: number | undefined = undefined): Promise<FeedEntry[]> { return this.pubsub.fetchFeed(communityJid, maxItems); }
  async publishFeedPost(communityJid: string, post: FeedPostInput): Promise<FeedEntry> { return this.pubsub.publishFeedPost(communityJid, post); }
  async fetchStories(communityJid: string, maxItems: number | undefined = undefined): Promise<Story[]> { return this.pubsub.fetchStories(communityJid, maxItems); }
  async fetchStoryReads(): Promise<StoryReads> { return this.pubsub.fetchStoryReads(); }
  async publishStoryReads(reads: StoryReads): Promise<StoryReads> { return this.pubsub.publishStoryReads(reads); }
  async publishStatusPreference(pref: StatusPreferenceWire): Promise<void> { return this.pubsub.publishStatusPreference(pref); }
  async fetchStatusPreference(): Promise<StatusPreferenceWire | null> { return this.pubsub.fetchStatusPreference(); }
  async publishStory(communityJid: string, input: StoryPostInput): Promise<Story> { return this.pubsub.publishStory(communityJid, input); }
  async fetchStoryReactions(communityJid: string, storyId: string): Promise<StoryReactionItem[]> { return this.pubsub.fetchStoryReactions(communityJid, storyId); }
  async fetchMyStoryReactions(communityJid: string, storyId: string): Promise<StoryReactionItem | null> { return this.pubsub.fetchMyStoryReactions(communityJid, storyId); }
  async fetchStoryReactionSummary(communityJid: string, storyId: string): Promise<StoryReactionSummary> { return this.pubsub.fetchStoryReactionSummary(communityJid, storyId); }
  async publishStoryReactions(communityJid: string, storyId: string, emojis: readonly string[], unknownChildrenXml: readonly string[] = []): Promise<void> { return this.pubsub.publishStoryReactions(communityJid, storyId, emojis, unknownChildrenXml); }
  async retractStoryReactions(communityJid: string, storyId: string): Promise<void> { return this.pubsub.retractStoryReactions(communityJid, storyId); }
  async fetchCommunityEvents(communityJid: string, maxItems: number | undefined = undefined): Promise<CommunityEvent[]> { return this.pubsub.fetchCommunityEvents(communityJid, maxItems); }
  async publishCommunityEvent(communityJid: string, input: CommunityEventInput): Promise<CommunityEvent> { return this.pubsub.publishCommunityEvent(communityJid, input); }
  async updateCommunityEvent(communityJid: string, itemId: string, input: CommunityEventInput): Promise<CommunityEvent> { return this.pubsub.updateCommunityEvent(communityJid, itemId, input); }
  async retractCommunityEvent(communityJid: string, itemId: string): Promise<void> { return this.pubsub.retractCommunityEvent(communityJid, itemId); }
  async rsvpCommunityEvent(communityJid: string, masterUid: string, selfLocalpart: string, selfBareJid: string, partstat: PartStat): Promise<void> { return this.pubsub.rsvpCommunityEvent(communityJid, masterUid, selfLocalpart, selfBareJid, partstat); }
  async publishMood(mood: MoodPublication): Promise<void> { return this.pubsub.publishMood(mood); }
  async retractMood(): Promise<void> { return this.pubsub.retractMood(); }
  async publishActivity(activity: ActivityPublication): Promise<void> { return this.pubsub.publishActivity(activity); }
  async retractActivity(): Promise<void> { return this.pubsub.retractActivity(); }
  async publishTune(tune: TunePublication): Promise<void> { return this.pubsub.publishTune(tune); }
  async retractTune(): Promise<void> { return this.pubsub.retractTune(); }
  async fetchUserPepProfile(jid: string): Promise<UserPepProfile> { return this.pubsub.fetchUserPepProfile(jid); }
  async fetchVCard4(jid: string): Promise<VCard4Profile | null> { return this.vcard.fetchVCard4(jid); }
  async publishVCard4(profile: VCard4Profile): Promise<void> { return this.vcard.publishVCard4(profile); }

  async queryMam(spaceId: string, channelId: string, max = 50): Promise<LiveRoomMessage[]> { return this.mam.queryMam(spaceId, channelId, max); }
  async queryMamPage(spaceId: string, channelId: string, max = 100, pageParam: MamPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> { return this.mam.queryMamPage(spaceId, channelId, max, pageParam); }
  async queryMamByThread(spaceId: string, channelId: string, threadId: string, max = 100): Promise<LiveRoomMessage[]> { return this.mam.queryMamByThread(spaceId, channelId, threadId, max); }
  async queryMamThreadPage(spaceId: string, channelId: string, threadId: string, max = 100, pageParam: MamThreadPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> { return this.mam.queryMamThreadPage(spaceId, channelId, threadId, max, pageParam); }
  async searchMessages(_spaceId: string, channelId: string, query: string, max = 20): Promise<MessageSearchResult[]> { return this.mam.searchMessages(channelId, query, max); }
  async queryPersonalMam(peerJid: string, max = 100, scope?: DmConversationScope): Promise<LiveDmMessage[]> { return this.mam.queryPersonalMam(peerJid, max, scope); }
  async queryPersonalMamPage(peerJid: string, max = 100, pageParam: MamPageParam = { type: "latest" }, scope?: DmConversationScope): Promise<MamHistoryPage<LiveDmMessage>> { return this.mam.queryPersonalMamPage(peerJid, max, pageParam, scope); }
  async queryPersonalMamThreadPage(peerJid: string, threadId: string, max = 100, pageParam: MamThreadPageParam = { type: "latest" }, scope?: DmConversationScope): Promise<MamHistoryPage<LiveDmMessage>> { return this.mam.queryPersonalMamThreadPage(peerJid, threadId, max, pageParam, scope); }
  async hydrateRecentDmCallActivity(peerJid: string, options: DmCallActivityHydrationOptions = {}): Promise<void> { return this.mam.hydrateRecentDmCallActivity(peerJid, options); }
  async hydrateRecentDmCallActivities(options: DmCallActivityHydrationOptions = {}): Promise<void> { return this.mam.hydrateRecentDmCallActivities(options); }
  async searchDmMessages(peerJid: string, query: string, max = 20, scope?: DmConversationScope): Promise<MessageSearchResult[]> { return this.mam.searchDmMessages(peerJid, query, max, scope); }
  async subscribeToPeerPresence(peerJid: string): Promise<void> { return this.presence.subscribeToPeerPresence(peerJid); }
  async setPresence(show: BroadcastShow, idleSince?: number | null): Promise<void> { return this.presence.setPresence(show, idleSince); }
  async listRosterContacts(): Promise<RosterContact[]> { const xmpp = await this.requireConnectedXmpp(); const roster = await xmpp.list_roster_contacts?.() as WasmRosterContact[]; return (roster ?? []).map((item) => { const jid = barePeerJid(item.jid); return { jid, name: item.name, username: item.name?.trim() || jidLocalpart(jid) || jid, subscription: (item.subscription ?? "none") as RosterContact["subscription"], groups: item.groups ?? [] }; }); }
  async getServerVersion(): Promise<WasmServerVersion | null> { const xmpp = await this.requireConnectedXmpp(); return await xmpp.get_server_version?.() as WasmServerVersion | null; }
  async discoverSpaceChannels(): Promise<DiscoveredChannel[]> { const xmpp = await this.requireConnectedXmpp(); return discoverChannels(xmpp as WasmClient, this.session.jid); }
  async createGroupDm(name: string, memberJids: string[]): Promise<CreateGroupDmResult> {
    const xmpp = await this.requireConnectedXmpp();
    return createGroupDm(xmpp as WasmClient, {
      userJid: this.session.jid,
      name,
      memberJids,
    });
  }
  async discoverTopology(): Promise<DiscoveredTopology> {
    const xmpp = await this.requireConnectedXmpp();
    const discoveryGeneration = ++this.roomDiscoveryGeneration;
    // A new refresh invalidates the previous observation until this run
    // proves each room's complete fingerprint again. A terminal denial
    // arriving while discovery is degraded or in flight must not inherit
    // an older pre-denial membership snapshot.
    this.currentRoomCatalogFingerprintEvidence.clear();
    const topology = await discoverTopology(xmpp as WasmClient, this.session.jid);
    if (
      this.destroying
      || this.xmpp !== xmpp
      || discoveryGeneration !== this.roomDiscoveryGeneration
    ) {
      return topology;
    }
    const authoritativeFingerprintFields = new Map(
      topology.roomReconciliationAuthority.roomFingerprints.map(
        ({ roomKey, fields }) => [roomKey, new Set(fields)] as const,
      ),
    );
    const absentRoomKeysAuthoritative = topology.roomCatalogComplete
      && topology.roomReconciliationAuthority.absentRoomKeysAuthoritative;
    const reconciliation = reconcileAutoJoinBlocks(
      this.terminallyDeniedAutoJoinRooms,
      topology.rooms,
      {
        absentRoomKeysAuthoritative,
        authoritativeFingerprintFields,
      },
    );
    this.terminallyDeniedAutoJoinRooms = reconciliation.blocks;
    for (const key of reconciliation.unblockedRoomKeys) {
      this.autoJoinAttemptedRoomKeys.delete(key);
      this.events.emit("roomAccessChanged", {
        roomJid: key,
        state: "available",
      });
    }
    if (reconciliation.changed) this.persistAutoJoinBlocks();
    this.updateRoomDiscoveryCaches(
      topology,
      authoritativeFingerprintFields,
      absentRoomKeysAuthoritative,
    );
    if (topology.services?.muc) this.mucServiceJid = barePeerJid(topology.services.muc);
    const autoJoinRoomJids = topology.rooms
      .filter((room) => {
        if (room.autojoin === false || !room.jid) return false;
        const key = this.roomJoinKey(room.jid);
        return topology.roomCatalogComplete
          || authoritativeFingerprintFields.get(key)?.has("autojoin");
      })
      .map((room) => room.jid!);
    if (autoJoinRoomJids.length > 0) void this.fanOutAutoJoin(autoJoinRoomJids);
    return topology;
  }

  private updateRoomDiscoveryCaches(
    topology: DiscoveredTopology,
    authoritativeFingerprintFields: ReadonlyMap<
      string,
      ReadonlySet<RoomCatalogFingerprintField>
    >,
    absentRoomKeysAuthoritative: boolean,
  ): void {
    const discoveredRoomEntries = topology.rooms.flatMap((room) =>
      room.jid ? [[room.id, room.jid] as const] : []
    );
    const authoritativeRooms = topology.rooms.filter((room) => {
      const key = room.jid ? this.roomJoinKey(room.jid) : "";
      return !!key && !!authoritativeFingerprintFields.get(key)?.size;
    });

    this.currentRoomCatalogFingerprintEvidence = new Map(
      authoritativeRooms.flatMap((room) => {
        const key = this.roomJoinKey(room.jid!);
        const fields = authoritativeFingerprintFields.get(key)!;
        return [[key, {
          catalogFingerprint: roomCatalogFingerprint(room),
          ...(hasCompleteRoomCatalogFingerprintAuthority(fields)
            ? {}
            : {
              catalogFingerprintFields:
                ROOM_CATALOG_FINGERPRINT_FIELDS.filter((field) =>
                  fields.has(field)
                ),
            }),
        }] as const];
      }),
    );

    if (absentRoomKeysAuthoritative) {
      this.discoveredRoomJids = new Map(discoveredRoomEntries);
      this.autoJoinRoomJids = topology.rooms
        .filter((room) => room.autojoin !== false && !!room.jid)
        .map((room) => room.jid!);
      return;
    }

    const discoveredRoomJids = new Map(this.discoveredRoomJids);
    const autoJoinRooms = new Map(
      this.autoJoinRoomJids.flatMap((roomJid) => {
        const key = this.roomJoinKey(roomJid);
        return key ? [[key, roomJid] as const] : [];
      }),
    );
    for (const room of authoritativeRooms) {
      const roomJid = room.jid!;
      const key = this.roomJoinKey(roomJid);
      discoveredRoomJids.set(room.id, roomJid);
      if (authoritativeFingerprintFields.get(key)?.has("autojoin")) {
        autoJoinRooms.delete(key);
        if (room.autojoin !== false) autoJoinRooms.set(key, roomJid);
      }
    }
    this.discoveredRoomJids = discoveredRoomJids;
    this.autoJoinRoomJids = [...autoJoinRooms.values()];
  }
  async listRoomMembers(channelId: string, options?: ListRoomMembersOptions): Promise<MemberSummary[]> { return this.mucAdmin.listRoomMembers(channelId, options); }
  async setRoomAffiliation(channelId: string, jid: string, affiliation: MemberSummary["affiliation"]): Promise<void> { return this.mucAdmin.setRoomAffiliation(channelId, jid, affiliation); }
  async isCommunityOwner(): Promise<boolean> { return this.mucAdmin.isCommunityOwner(); }
  async adminUsersList(opts: { prefix?: string | null; pageSize?: number | null; afterCursor?: string | null } = {}): Promise<AdminUsersPage> { return this.mucAdmin.adminUsersList(opts); }
  async adminSpacesList(opts: { prefix?: string | null; pageSize?: number | null; afterCursor?: string | null } = {}): Promise<WasmAdminSpacesListResult> { return this.mucAdmin.adminSpacesList(opts); }
  async adminSpacesCreate(opts: { name: string; description?: string | null; iconUrl?: string | null }): Promise<WasmAdminSpaceRef> { return this.mucAdmin.adminSpacesCreate(opts); }
  async adminSpacesUpdate(opts: { spaceJid: string; spaceNode?: string | null; name?: string | null; description?: string | null; iconUrl?: string | null }): Promise<WasmAdminSpaceRef> { return this.mucAdmin.adminSpacesUpdate(opts); }
  async adminSpacesDelete(opts: { spaceJid: string; spaceNode?: string | null }): Promise<boolean> { return this.mucAdmin.adminSpacesDelete(opts); }
  async adminSpacesMembers(opts: { spaceJid: string; spaceNode?: string | null; pageSize?: number | null; afterCursor?: string | null }): Promise<WasmAdminSpacesMembersResult> { return this.mucAdmin.adminSpacesMembers(opts); }
  async adminSpacesSetRole(opts: { spaceJid: string; spaceNode?: string | null; memberJid: string; role: "owner" | "admin" | "member" | "none" }): Promise<WasmAdminSpacesSetRoleResult> { return this.mucAdmin.adminSpacesSetRole(opts); }
  async adminChannelsList(opts: { spaceJid?: string | null; spaceNode?: string | null; prefix?: string | null; pageSize?: number | null; afterCursor?: string | null } = {}): Promise<WasmAdminChannelsListResult> { return this.mucAdmin.adminChannelsList(opts); }
  async adminChannelsCreate(opts: { name: string; topic?: string | null; channelType?: WasmAdminChannelType | null; spaceJid?: string | null; spaceNode?: string | null; isPublic?: boolean | null; membersOnly?: boolean | null }): Promise<WasmAdminChannelRef> { return this.mucAdmin.adminChannelsCreate(opts); }
  async adminChannelsUpdate(opts: { channelJid: string; name?: string | null; topic?: string | null; channelType?: WasmAdminChannelType | null; isPublic?: boolean | null; membersOnly?: boolean | null }): Promise<WasmAdminChannelRef> { return this.mucAdmin.adminChannelsUpdate(opts); }
  async adminChannelsDelete(opts: { channelJid: string }): Promise<boolean> { return this.mucAdmin.adminChannelsDelete(opts); }
  async adminChannelsOccupants(opts: { channelJid: string; pageSize?: number | null; afterCursor?: string | null }): Promise<WasmAdminChannelsOccupantsResult> { return this.mucAdmin.adminChannelsOccupants(opts); }
  async adminChannelsAffiliations(opts: { channelJid: string; filter?: string | null; pageSize?: number | null; afterCursor?: string | null }): Promise<WasmAdminChannelsAffiliationsResult> { return this.mucAdmin.adminChannelsAffiliations(opts); }
  async adminChannelsSetAffiliation(opts: { channelJid: string; memberJid: string; affiliation: "owner" | "admin" | "member" | "none" | "outcast"; reason?: string | null }): Promise<WasmAdminChannelsSetAffiliationResult> { return this.mucAdmin.adminChannelsSetAffiliation(opts); }
  async adminChannelsKick(opts: { channelJid: string; occupantJid: string; reason?: string | null }): Promise<WasmAdminChannelsKickResult> { return this.mucAdmin.adminChannelsKick(opts); }
  async searchUsers(query: string): Promise<UserSearchResult[]> { if (!query.trim()) return []; const xmpp = await this.requireConnectedXmpp(); const users = await xmpp.search_users?.(query) as WasmUserSearchResult[]; return (users ?? []).map((user) => ({ id: user.jid, jid: user.jid, username: user.username ?? user.nick ?? jidLocalpart(user.jid), display_name: user.display_name ?? user.name ?? null, avatar_url: null })); }
  async fetchUserAvatar(jid: string): Promise<string | null> { return this.vcard.fetchUserAvatar(jid); }
  get agent(): XmppClientInstance | null { return this.xmpp; }

  private startSelfPing() { this.stopSelfPing(); this.selfPingTimer = setInterval(() => { void this.doSelfPing(); }, 60000); }
  private stopSelfPing() { if (this.selfPingTimer) { clearInterval(this.selfPingTimer); this.selfPingTimer = null; } }
  private async doSelfPing() { if (!this.xmpp?.send_raw_iq || !this.currentRoom) return; try { await this.xmpp.send_raw_iq(`<iq type="get" id="${crypto.randomUUID()}" to="${this.currentRoom}/${this.session.username}"><ping xmlns="urn:xmpp:ping"/></iq>`); } catch { this.events.emit("roomDisconnect"); } }
  private handleMessageAck(id: string) { this.outboundQueue.handleAck(id); }
  private handleMessageFailed(id: string) { this.outboundQueue.handleFailed(id); }
  // `lifecycle` is the bare kind here — the emitted
  // `SessionLifecycleEvent` is built inside `runSessionReady`, where
  // the fresh variant gains its reconnect catch-up coverage (#1180).
  private handleSessionReady(xmpp: XmppClientInstance, lifecycle: { type: SessionLifecycleEvent["type"] }) {
    void this.runSessionReady(xmpp, lifecycle);
  }

  private async runSessionReady(xmpp: XmppClientInstance, lifecycle: { type: SessionLifecycleEvent["type"] }) {
    if (this.xmpp !== xmpp) return;
    this.callTransportRecovery.onTransportReady();
    this.connected = true; this.reconnect.resetAttempts();
    this.emitStatus({ state: "online", detail: this.outboundQueue.persistedCount() > 0 ? lifecycle.type === "fresh" ? "Reconnected — replaying queued messages" : "Connection resumed — replaying queued messages" : lifecycle.type === "fresh" ? "Connection ready" : "Connection resumed" });
    // Coalesce duplicate triggers. Three event hooks call
    // `handleSessionReady` on the same xmpp handle; only the first
    // gets past this latch to run the per-session setup, lifecycle
    // emit, and catch-up. Subsequent triggers for the *same* handle
    // bail out silently.
    //
    // The latch is keyed on the handle, set once and synchronously
    // (before the first await), so it also closes the no-catch-up leak
    // (#1221): that branch returns without opening a `resumeBarrier`, so
    // a barrier-only gate let a second callback re-admit → double
    // fan-out + double bootstrap. A *different* handle (e.g. the old one
    // from a Wi-Fi → cellular reconnect) does not match the latch — the
    // new handle gets its own session-setup + catch-up.
    if (this.sessionReadyHandledXmpp === xmpp) return;
    this.sessionReadyHandledXmpp = xmpp;
    if (lifecycle.type === "fresh") {
      this.outboundQueue.clearOrdinaryInflight();
      void this.enableCarbons(xmpp);
      // XEP-0490 §3.1 + §3.2: catch up displayed state and subscribe
      // to future +notify events. Both are best-effort and fully
      // fail-silent (chat works without MDS). Bound to the specific
      // xmpp reference so the bootstrap aborts cleanly if the client
      // disconnects before the catch-up IQ resolves.
      void this.bootstrapMdsDisplayed(xmpp);
      // A fresh bind lost server-side MUC occupancy, so re-send join
      // presence for every retained/autojoin room + the focused room.
      // Single-flight per epoch (`fanOutAutoJoin`) keeps this to one
      // join per room across the three fan-out triggers (#1221).
      this.fanOutJoinableRooms();
    } else {
      // Resumed: occupancy survived the SM detach-for-resume server-side
      // (no MUC leave was broadcast). Re-seed readiness for the
      // self-presence-confirmed rooms WITHOUT sending presence (they mark
      // themselves attempted), then fan out — single-flight skips the
      // reseeded rooms, so a confirmed room sends zero join while any
      // room the snapshot did NOT confirm is still rejoined (#1221).
      // This covers a room still mid-join at disconnect (not yet 110-
      // confirmed) and a lost snapshot after a page reload (the pagehide
      // handoff persists SM state, not the in-memory snapshot). One
      // client rejoining its own rooms is not the multi-client storm
      // #1221 fixes.
      this.reseedResumedRooms();
      this.fanOutJoinableRooms();
    }
    // #1180: consume the catch-up cursors BEFORE emitting the
    // lifecycle event so the fresh event can report which
    // conversations `runReconnectCatchup` is about to page. Timeline
    // consumers skip their own wholesale MAM reload for covered
    // conversations — otherwise two concurrent fetches race to write
    // the timeline and the rebuild clobbers the catch-up's merges.
    const catchupEntries = this.catchup.onSessionStarted();
    this.emitSessionLifecycle(
      lifecycle.type === "fresh"
        ? {
          type: "fresh",
          catchup: {
            dmJids: catchupEntries.filter((e) => e.kind === "dm").map((e) => e.key),
            ...(catchupEntries.some((e) => e.kind === "dm" && e.scope === "muc-occupant")
              ? { dmOccupantJids: catchupEntries.filter((e) => e.kind === "dm" && e.scope === "muc-occupant").map((e) => e.key) }
              : {}),
            roomJids: catchupEntries.filter((e) => e.kind === "room").map((e) => e.key),
          },
        }
        : { type: "resumed" },
    );
    if (catchupEntries.length === 0) {
      // No barrier this session: entries carried from a failed barrier
      // still owe their dispatch (same flush-then-drain order).
      const carried = this.carriedPendingDuringResume.splice(0);
      this.flushAfterSessionReady(xmpp);
      if (carried.length > 0) this.drainResumeBuffer(carried);
      this.fulfillOnceConnected();
      return;
    }
    const catchupPromise = this.runReconnectCatchup(xmpp, catchupEntries, lifecycle.type);
    const barrierPromise = catchupPromise.finally(() => this.completeResumeBarrier(xmpp));
    this.openResumeBarrier(xmpp, barrierPromise);
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
  /**
   * Open the resume barrier: build the buffer and the barrier *atomically*
   * — both fields set together with no `await` in between, so a re-entrant
   * `handleSessionReady` (synchronous WASM callback) can never observe
   * `pendingDuringResume` set without `resumeBarrier` also set. Seeds the
   * buffer with entries carried over from a barrier that failed
   * mid-catch-up (F2) so they drain with this barrier, ahead of newer
   * arrivals.
   */
  private openResumeBarrier(xmpp: XmppClientInstance, promise: Promise<void>) {
    this.pendingDuringResume = this.carriedPendingDuringResume.splice(0);
    this.resumeBarrier = { xmpp, promise };
  }

  private completeResumeBarrier(xmpp: XmppClientInstance) {
    const buffered = this.pendingDuringResume ?? [];
    // Only clear the barrier if it is still ours — a newer handle
    // may have replaced it after a full reconnect.
    const ownBarrier = this.resumeBarrier?.xmpp === xmpp;
    if (ownBarrier) {
      this.resumeBarrier = null;
      this.pendingDuringResume = null;
    }
    if (this.xmpp !== xmpp) {
      // F2: our barrier failed (the connection died mid-catch-up). The
      // buffered stanzas were SM-acked and will never be replayed —
      // carry them into the next barrier instead of discarding them.
      // (When the barrier was NOT ours, `buffered` is the newer
      // barrier's live buffer — leave it alone.)
      if (ownBarrier && buffered.length > 0) {
        this.carriedPendingDuringResume.push(...buffered);
      }
      return;
    }
    // Flush the locally-queued outbound *before* draining the live
    // buffer. Queued messages carry the user's send wall-clock from
    // before the pause; live arrivals from during the pause carry
    // later stamps. Flushing first means the cursor advances in
    // chronological order and `mergeLiveMessage` sees outbound
    // echoes alongside (or before) the inbound arrivals they belong
    // next to, instead of mis-ordering the tail (Bug 4).
    this.flushAfterSessionReady(xmpp);
    this.drainResumeBuffer(buffered);
  }

  private drainResumeBuffer(buffered: InboundWasmMessage[]) {
    // Drain bodies first, then targeted follow-ups (displayed markers
    // + reactions), each kind in arrival order (#1165). A follow-up
    // whose target arrived during the same catch-up window — via MAM
    // pagination (already merged: `runReconnectCatchup` completed
    // before this drain) or via the live buffer itself — must see its
    // target row already inserted, or `applyReactionUpdate` /
    // `applyDisplayedMarker` short-circuit on the miss and the merge
    // layers drop it silently. Each replay flows through the same
    // `dispatchLiveMessage` path a fresh socket arrival would, so
    // cursor advance + downstream listener dedup behave identically.
    // Observe-only: the buffer drain is a single synchronous task over
    // everything that arrived during the resume barrier — a prime
    // background-tab HUNG suspect. Measure its size + wall-clock.
    const drainStartedAt = performance.now();
    const isTargetedFollowUp = (m: InboundWasmMessage) => !!m.displayed_marker_id || !!m.reaction_target_id;
    for (const m of buffered) if (!isTargetedFollowUp(m)) this.dispatchLiveMessage(m);
    for (const m of buffered) if (isTargetedFollowUp(m)) this.dispatchLiveMessage(m);
    this.events.emitSafe("resumeDrain", {
      buffered: buffered.length,
      durationMs: performance.now() - drainStartedAt,
    });
  }

  private flushAfterSessionReady(xmpp: XmppClientInstance) {
    if (this.xmpp !== xmpp) return;
    void this.flushQueuedDirectMessages();
    const roomJid = this.currentRoom;
    if (roomJid) void this.flushQueuedRoomAfterJoin(xmpp, roomJid);
  }

  private async flushQueuedRoomAfterJoin(xmpp: XmppClientInstance, roomJid: string): Promise<void> {
    if (this.roomIsTerminallyDenied(roomJid)) return;
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
        if (raw) {
          for (const entry of raw) {
            this.events.emit("mdsDisplayed", { chatId: entry.chat_id, stanzaId: entry.stanza_id, stanzaIdBy: entry.stanza_id_by });
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
  private teardownCallAfterTransportLoss(): void {
    clearCallState({ endReason: "error" });
    clearMucCallParticipants();
    clearAllRaisedHands();
    clearAllMuted();
    clearAllLiveCallParticipants();
    void useCallEngine().engine.disconnect();
  }

  private handleDisconnected(xmpp: XmppClientInstance, error?: Error) {
    if (this.xmpp !== xmpp) return;
    this.roomJoinRetry.cancelAll();
    const previouslyJoinedRooms = [...this.joinedMucs.keys()];
    // Snapshot the self-presence-confirmed (status 110) rooms before the
    // trackers clear so a subsequent `resumed` session-ready can restore
    // readiness without re-sending join presence (#1221).
    this.resumedSessionRoomKeys = new Set(this.joinedMucReady);
    this.connected = false; this.stopSelfPing(); this.xmpp = null;
    // Established media is independent of the transient XMPP transport.
    // Preserve its UI projections while stream recovery has a bounded
    // chance to restore the signalling plane; setup phases still fail fast.
    // A TERMINAL classification (auth rejection, resource conflict — the
    // branch below that schedules no reconnect) or an already-latched
    // terminal state makes recovery impossible: tear down now instead of
    // leaving the room and capture alive for the full grace window.
    const callTeardownDeferred = !this.destroying
      && this.terminalDisconnectDetail === null
      && !this.inTerminalErrorState
      && this.callTransportRecovery.onTransportLost() === "deferred";
    if (!callTeardownDeferred) this.teardownCallAfterTransportLoss();
    this.rejectRoomJoinWaiters(new Error("XMPP disconnected while joining a room"));
    // `joinedMucs` keys are already canonical `roomJoinKey`s (#1221).
    this.retainedJoinedRoomJids = new Set([
      ...this.retainedJoinedRoomJids,
      ...previouslyJoinedRooms.filter(Boolean),
    ]);
    this.persistRetainedJoinedRooms();
    this.joinedMucs.clear();
    this.joinedMucJoinTokens.clear();
    this.joinedMucReady.clear();
    // New epoch: a genuine fresh cycle must be free to rejoin (#1221),
    // and the next handle must run its own session-ready setup.
    this.autoJoinAttemptedRoomKeys.clear();
    this.roomDiscoveryGeneration += 1;
    this.currentRoomCatalogFingerprintEvidence.clear();
    this.sessionReadyHandledXmpp = null;
    this.clearRoomPresenceCaches();
    if (this.destroying) { this.clearResumeState(); this.emitStatus({ state: "offline", detail: error?.message ?? "Disconnected" }); return; }
    // #1164: a terminal failure (auth rejection, resource conflict)
    // means retrying is dishonest — surface `state: "error"` so the
    // connection notice tells the user to sign in again, and do NOT
    // schedule a reconnect. The classification comes exclusively from
    // the preceding `set_on_error` callback (the WASM core reports
    // stream errors structured, and connect-time SASL failures as the
    // driver's ClientError display string — both land there BEFORE the
    // disconnect callback, which itself carries no error). C4: never
    // word-scan the disconnect error's free-text message — a proxied
    // close reason like "409 Conflict" must not permanently kill
    // reconnection.
    const terminalDetail = this.terminalDisconnectDetail;
    if (terminalDetail) {
      this.terminalDisconnectDetail = null;
      // F1: latch the terminal state so background `connect()` calls
      // cannot silently exit the "sign in again" error surface.
      this.inTerminalErrorState = true;
      this.emitStatus({ state: "error", detail: terminalDetail });
      if (this.onceConnectFailed) { const fail = this.onceConnectFailed; this.onceConnected = null; this.onceConnectFailed = null; fail(error ?? new Error(terminalDetail)); }
      return;
    }
    // Keep transient reconnect state in this JS context only — see
    // `ResumeStateStore.captureFromDisconnect` for the pagehide-handoff
    // rationale.
    const resumeState = this.resume.captureFromDisconnect(xmpp, this.resource);
    this.outboundQueue.seedFromResumeState(resumeState);
    this.emitStatus({ state: "reconnecting", detail: this.outboundQueue.persistedCount() > 0 ? "Connection lost — queued messages will send when reconnected" : (error?.message ?? "Connection lost, reconnecting...") });
    if (this.onceConnectFailed) { const fail = this.onceConnectFailed; this.onceConnected = null; this.onceConnectFailed = null; fail(error ?? new Error("XMPP connection failed")); }
    this.reconnect.schedule();
  }
  private handlePresence(presence: WasmPresence) {
    this.presence.handle(presence);
  }
  private handleMessage(message: InboundWasmMessage) {
    const inboxPush = message.inboxPush ?? (message.inbox_push ? inboxEntryFromWasm(message.inbox_push) : undefined);
    if (inboxPush) { this.events.emit("inboxPush", inboxPush); return; }
    if (message.carbon?.sent || message.carbon?.received) {
      // XEP-0280 (#1243): the WASM core unwrapped a verified carbon and
      // this IS the inner message.
      //
      // A carbon-SENT chat state or displayed marker mirrors OUR OWN
      // activity on another device; surfacing it as peer state would
      // render "peer is typing" / "peer read this" for ourselves.
      // (Cross-device read sync travels via XEP-0490 MDS instead.)
      // Dropped BEFORE any dedupe bookkeeping so a typing burst can't
      // flood the bounded set and evict ids still guarding real rows.
      if (message.carbon.sent && ((message.chat_state && !message.body) || message.displayed_marker_id)) return;
      // Remember row-producing carbon ids (scoped by sender — stanza
      // ids are only unique per sender) so a duplicate direct delivery
      // of the same stanza drops below, and an SM-replayed copy of the
      // carbon itself drops here. Ephemeral chat states never enter
      // the set.
      const dedupeKey = this.carbonDedupKey(message);
      if (dedupeKey) {
        const seenAs = this.carbonDedupIds.get(dedupeKey);
        if (seenAs === "carbon") return; // SM-replayed carbon copy
        if (seenAs === "direct") {
          // The DIRECT copy arrived first and rendered with a live
          // fallback timestamp. If this carbon carries the forwarded
          // <delay/>, let it through: the merge layer collapses it by
          // wire id and `pickAuthoritativeTimestamp` upgrades the
          // fallback stamp — dropping it would strand the row on
          // `Date.now()` (#1267 item 6). Timestamp-less duplicates drop.
          // The pass-through is restamp-ONLY: the direct copy already
          // incremented unread and fired notifications.
          this.rememberCarbonId(dedupeKey, "carbon");
          if (!message.timestamp) return;
          message = { ...message, restampOnly: true };
        } else {
          this.rememberCarbonId(dedupeKey, "carbon");
        }
      }
    } else {
      const dedupeKey = this.carbonDedupKey(message);
      if (dedupeKey) {
        if (this.carbonDedupIds.has(dedupeKey)) {
          // Duplicate/replayed copy — drop, but KEEP the entry: deleting
          // it here would let an SM-replayed carbon of the same stanza
          // look unseen and dispatch again (double unread/notification).
          // The bounded map evicts entries by age instead.
          return;
        }
        // Remember direct deliveries too, so the pair collapses in
        // BOTH orders (direct-then-carbon as well as carbon-then-
        // direct) and an SM-replayed direct copy drops like a
        // replayed carbon does.
        this.rememberCarbonId(dedupeKey, "direct");
      }
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
    if (message.chat_state && !message.body) {
      if (message.is_muc) { const roomJid = barePeerJid(message.from ?? message.to ?? ""); const nick = (message.from ?? "").split("/")[1] ?? "unknown"; if (roomJid === this.currentRoom && nick !== this.session.username) this.events.emit("chatState", { roomJid, nick, state: message.chat_state as ChatStateType }); }
      else {
        // XEP-0045 §7.5 (#1256): a chat state from a joined room's
        // occupant JID belongs to that occupant's PM conversation, not
        // to a phantom DM keyed by the room bare JID.
        const occupant = this.mucPmOccupant(message);
        this.events.emit("dmChatState", { peerJid: occupant?.occupantJid ?? barePeerJid(message.from ?? message.to ?? ""), state: message.chat_state as ChatStateType });
      }
      return;
    }
    if (message.in_call?.kind === "reaction") {
      if (!isInCallReactionForActiveCall($callState.get(), message.in_call.sid)) return;
      receiveInCallReaction({
        sid: message.in_call.sid,
        emoji: message.in_call.emoji,
        from: message.from ?? "",
      });
      return;
    }
    if (message.pin_event) {
      const roomJid = message.is_muc
        ? barePeerJid(message.from ?? message.to ?? "")
        : this.directPeerFromMessage(message);
      if (roomJid) this.events.emit("pinEvent", { roomJid, event: message.pin_event });
      if (!message.is_muc) return;
      // Fall through: also render the system message in the timeline so
      // the user sees "alice pinned a message" inline (#414). The
      // pin store update has already happened above.
    }
    if (this.pendingDuringResume !== null) {
      this.pendingDuringResume.push(message);
      return;
    }
    this.dispatchLiveMessage(message);
  }

  /**
   * Dispatch a live inbound message past the resume gate: targeted
   * follow-ups (XEP-0333 displayed markers, XEP-0444 reactions) emit
   * their dedicated events; everything else is a body message. Called
   * both from `handleMessage` (no barrier) and from the barrier drain
   * in `completeResumeBarrier` — it never re-enters the buffer.
   */
  private dispatchLiveMessage(message: InboundWasmMessage) {
    if (message.displayed_marker_id) { if (message.is_muc) { const roomJid = barePeerJid(message.from ?? message.to ?? ""); const nick = (message.from ?? "").split("/")[1] ?? "unknown"; this.events.emit("displayed", { roomJid, nick, messageId: message.displayed_marker_id }); } else { const occupant = this.mucPmOccupant(message); this.events.emit("dmDisplayed", { peerJid: occupant?.occupantJid ?? barePeerJid(message.from ?? message.to ?? ""), messageId: message.displayed_marker_id }); } return; }
    if (message.reaction_target_id) { const occurredAt = message.timestamp ? { occurredAt: message.timestamp } : {}; if (message.is_muc) { const roomJid = barePeerJid(message.from ?? message.to ?? ""); const nick = (message.from ?? "").split("/")[1] ?? "unknown"; this.events.emit("reaction", { roomJid, nick, messageId: message.reaction_target_id, emojis: message.reaction_emojis, ...occurredAt }); } else { const occupant = this.mucPmOccupant(message); const fromBare = barePeerJid(message.from ?? ""); const toBare = barePeerJid(message.to ?? ""); const selfBare = barePeerJid(this.session.jid); const peerJid = occupant?.occupantJid ?? (fromBare === selfBare ? toBare : fromBare); const reactorJid = (occupant && fromBare !== selfBare ? occupant.occupantJid : fromBare) || selfBare; if (peerJid && reactorJid) this.events.emit("dmReaction", { peerJid, reactorJid, messageId: message.reaction_target_id, emojis: message.reaction_emojis, ...occurredAt }); } return; }
    this.dispatchLiveBodyMessage(message);
  }

  private dispatchLiveBodyMessage(message: InboundWasmMessage) {
    if (message.is_muc) {
      const converted = roomMessageFromArchived({ ...message, mam_id: message.id ?? crypto.randomUUID() } as WasmArchivedMessage, "live", { trustedMediaOrigin: this.trustedLinkPreviewMediaOrigin() });
      if (!converted) return;
      // Deliberately no archive-cursor advance here: a live stanza-id
      // cannot prove delivery continuity (MUC gap during a kick/rejoin,
      // SM replay-window eviction), so catch-up re-fetches from the last
      // archive-CONFIRMED cursor and filters via `seenIds` instead.
      this.catchup.recordRoomSeen(converted.roomJid, converted.createdAt, undefined, rawMessageSeenIds(message, [converted.roomJid]));
      if (converted.roomJid !== this.currentRoom && isRoomActivityMessage(converted)) { this.events.emit("activity", roomActivityEventFromMessage(converted)); return; }
      this.events.emit("message", converted); return;
    }
    const selfBare = barePeerJid(this.session.jid);
    const converted = dmMessageFromArchived({ ...message, mam_id: message.id ?? crypto.randomUUID() } as WasmArchivedMessage, selfBare, "live", { trustedMediaOrigin: this.trustedLinkPreviewMediaOrigin() });
    if (converted) {
      // XEP-0045 §7.5 (#1256): a `type='chat'` message whose counterpart
      // is `room@service/nick` for a known room is a MUC private message.
      // Re-key the conversation to the occupant JID so it never misfiles
      // under the room bare JID (where a reply would broadcast).
      const occupant = this.mucPmOccupant(message);
      if (occupant) {
        converted.peerJid = occupant.occupantJid;
        converted.mucPm = true;
        if (barePeerJid(message.from ?? "") !== selfBare) converted.nick = occupant.nick;
      }
      if (message.restampOnly) converted.timestampRefreshOnly = true;
      // XEP-0359: the DM authorities mirror the decode path
      // (`assignedStanzaIdBy`): account bare + server domain. Seen-ids
      // and row wireIds must never diverge (see dmStanzaIdAuthorities).
      this.catchup.recordDmSeen(
        converted.peerJid,
        converted.createdAt,
        undefined,
        rawMessageSeenIds(message, [selfBare, jidDomain(selfBare)]),
        converted.mucPm ? "muc-occupant" : "account",
      );
      this.events.emit("directMessage", converted);
    }
  }

  private directPeerFromMessage(message: InboundWasmMessage): string {
    const selfBare = barePeerJid(this.session.jid);
    const fromBare = barePeerJid(message.from ?? "");
    const toBare = barePeerJid(message.to ?? "");
    return fromBare === selfBare ? toBare : fromBare;
  }

  /**
   * XEP-0280 dedupe key for a carbon-copied stanza and its potential
   * duplicate direct delivery: sender-scoped (stanza ids are only
   * unique per sender, so a bare `id` key would let Bob's carbon
   * swallow Carol's unrelated message). `undefined` for stanzas that
   * never produce a duplicate pair (groupchat, id-less, chat-state-only
   * ephemera).
   */
  private carbonDedupKey(message: InboundWasmMessage): string | undefined {
    if (message.is_muc || !message.id) return undefined;
    if (message.chat_state && !message.body) return undefined;
    // Sender scope keeps the folded bare (case-insensitive per RFC 7622)
    // but preserves the resource verbatim: MUC-PM senders are occupant
    // JIDs, and two occupants of the same room must never share a key —
    // with direct deliveries remembered too, a bare-folded key would let
    // one occupant's id swallow another occupant's message.
    const from = message.from ?? "";
    const bare = bareJidKey(from);
    // No sender identity → no sender-scoped key. A `|<id>` key would
    // collide across unrelated from-less stanzas and mis-drop them.
    if (!bare) return undefined;
    const slash = from.indexOf("/");
    const sender = slash >= 0 ? `${bare}/${from.slice(slash + 1)}` : bare;
    return `${sender}|${message.id}`;
  }

  /** Bounded XEP-0280 dedupe memory: a sender-scoped wire id is
   * remembered (tagged with which representation arrived) so the
   * carbon/direct pair collapses; capped so the map cannot grow
   * monotonically over a long session. */
  private rememberCarbonId(key: string, source: "carbon" | "direct") {
    this.carbonDedupIds.delete(key);
    this.carbonDedupIds.set(key, source);
    if (this.carbonDedupIds.size > 512) {
      const oldest = this.carbonDedupIds.keys().next().value;
      if (oldest !== undefined) this.carbonDedupIds.delete(oldest);
    }
  }

  /** Whether `bareJid` is a MUC room this session knows about (joined,
   * retained across reconnect, or discovered via space topology). */
  private isKnownMucRoomBare(bareJid: string): boolean {
    const key = this.roomJoinKey(bareJid);
    if (!key) return false;
    if (this.joinedMucs.has(key) || this.retainedJoinedRoomJids.has(key)) return true;
    for (const roomJid of this.discoveredRoomJids.values()) {
      if (this.roomJoinKey(roomJid) === key) return true;
    }
    return false;
  }

  private isMucServiceOccupant(peerJid: string): boolean {
    return Boolean(resourceOf(peerJid))
      && jidDomain(peerJid).toLowerCase() === this.mucServiceJid.trim().toLowerCase();
  }

  /**
   * XEP-0045 §7.5 (#1256): detect a MUC private message — a non-groupchat
   * message whose conversation counterpart is `room@service/nick` for a
   * known room. Returns the occupant JID (the conversation identity a
   * reply must address) and the occupant nick, or `undefined` for a
   * normal 1:1 message.
   */
  private mucPmOccupant(message: InboundWasmMessage): { occupantJid: string; nick: string } | undefined {
    if (message.is_muc) return undefined;
    const selfBare = barePeerJid(this.session.jid);
    const from = message.from ?? "";
    const counterpart = barePeerJid(from) === selfBare ? (message.to ?? "") : from;
    // The nick is the FULL resource — XEP-0045 nicks may themselves
    // contain '/', so split-once, never split-all.
    const slash = counterpart.indexOf("/");
    const nick = slash >= 0 ? counterpart.slice(slash + 1) : "";
    if (!nick || !this.isKnownMucRoomBare(barePeerJid(counterpart))) return undefined;
    return { occupantJid: counterpart, nick };
  }

  /** Public: whether `bareJid` is a MUC room this session knows about.
   * Consumed by the DM conversations store to keep occupant-keyed MUC-PM
   * conversations from folding to (or being duplicated under) the room
   * bare JID (#1256). */
  isKnownMucRoom(bareJid: string): boolean {
    return this.isKnownMucRoomBare(barePeerJid(bareJid));
  }

  /** Public: whether `peerJid` is a full occupant JID proven by the
   * configured MUC service, a discovered room, or persisted catch-up scope. */
  isMucPmPeer(peerJid: string): boolean {
    if (!resourceOf(peerJid)) return false;
    return this.isMucServiceOccupant(peerJid)
      || this.isKnownMucRoomBare(barePeerJid(peerJid))
      || this.catchup.getDmScope(peerJid) === "muc-occupant";
  }
  private runReconnectCatchup(
    xmpp: XmppClientInstance,
    entries: ReadonlyArray<ReconnectCatchupEntry>,
    lifecycle: SessionLifecycleEvent["type"],
  ) {
    return this.mam.runReconnectCatchup(xmpp, entries, lifecycle);
  }
  private wireEvents(xmpp: XmppClientInstance & { enableKeepAlive?: (opts: { interval: number; timeout: number }) => void; disableKeepAlive?: () => void }) {
    if (!this.xmpp && !this.destroying) this.xmpp = xmpp;
    // #754: carbons enable lives in the fresh branch of `runSessionReady`
    // only — enabling from `on_connected` too doubled the IQ on every
    // fresh connect and re-enabled on XEP-0198 resume, where the server
    // already preserves carbon state for the resumed session.
    xmpp.set_on_session_lifecycle?.((event: string) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      if (event === "resumed") this.handleSessionReady(xmpp, { type: "resumed" }); else this.handleSessionReady(xmpp, { type: "fresh" });
    });
    xmpp.set_on_disconnected?.(() => this.handleDisconnected(xmpp));
    xmpp.set_on_error?.((payload: XmppStreamErrorPayload) => {
      if (this.xmpp !== xmpp) return;
      const streamError = normalizeXmppStreamErrorPayload(payload);
      const terminalDetail = terminalDisconnectDetail(terminalConditionFromPayload(payload));
      if (terminalDetail) this.terminalDisconnectDetail = terminalDetail;
      this.emitError({
        kind: "stream",
        recoverable: !this.destroying && !terminalDetail,
        detail: streamError.detail,
        ...(streamError.condition ? { condition: streamError.condition } : {}),
        ...(streamError.streamManagementError
          ? { streamManagementError: streamError.streamManagementError }
          : {}),
      });
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
      // #1029/#1030: the raised-hand and mute `<in-call>` presence states
      // ride the same stanza; mirror each into its per-room store. Mute is
      // the authoritative remote-mute source (replaces LiveKit signalling).
      applyRaisedHandPresence(presence);
      applyMutePresence(presence);
    });
    xmpp.set_on_message_delivery_acked?.((id: string) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.handleMessageAck(id);
    });
    xmpp.set_on_message_delivery_failed?.((id: string) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.handleMessageFailed(id);
    });
    xmpp.set_on_stream_management?.((event: StreamManagementTelemetry) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.events.emitSafe("streamManagement", event);
    });
    xmpp.set_on_mds_displayed?.((entry: WasmMdsDisplayedEntry) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.events.emit("mdsDisplayed", { chatId: entry.chat_id, stanzaId: entry.stanza_id, stanzaIdBy: entry.stanza_id_by });
    });
    xmpp.set_on_pubsub_event?.((event: WasmPubsubEvent) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.events.emit("pubsubEvent", event);
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
        event.kind === "ringing" ||
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
        applyCallEvent(event, {
          sender: xmpp as unknown as CallWireSender,
          selfOriginated: isSelfOriginated,
          selfFullJid: this.fullJid,
        });
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
    xmpp.on?.("presence", (presence: WasmPresence) => {
      if (!this.isCurrentXmpp(xmpp)) return;
      this.handlePresence(presence);
      applyMucCallPresence(presence);
      applyRaisedHandPresence(presence);
      applyMutePresence(presence);
    });
  }
}
