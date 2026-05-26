<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch, type ComponentPublicInstance } from "vue";
import { useStore } from "@nanostores/vue";
import { AlertCircle, CheckCircle2, Hash, Loader2, MessageCircle, MessagesSquare, RefreshCw, Search, SearchX, Upload, WifiOff, X } from "lucide-vue-next";
import { $pinnedStanzaIds } from "@/stores/pinned-messages";
import { isForumChannel as detectForumChannel } from "@/lib/channel-types";
import type { NotifySettingsStore } from "@/lib/notify-settings";
import { getConnectionNoticeCopy } from "@/lib/connection-notice";
import { findMessageElementById } from "@/lib/message-targeting";
import { findMessageById } from "@/lib/message-ids";
import { getReplyJumpNotice } from "@/lib/reply-ux";
import { findThreadToAutoOpen } from "@/lib/thread-auto-open";
import {
  getPinnedScrollTop,
  getNewMessagesDividerPlacement,
  orderTimelineForScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";
import { extractFilesFromEvent } from "@/lib/xmpp/file-upload";
import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { ExtensionAnnotationAction, TimelineMessage, MarkupSpan, MessageReference } from "@/lib/chat-ui";
import type { CallMedia } from "@/lib/calls/types";
import type { MentionCandidate } from "@/lib/mentions";
import type { BrowserXmppClient, MessageSearchResult, XmppStatusSnapshot, RoomAuthority, RoomHats, RoomPresence } from "@/lib/xmpp-client";
import type { MemberLoadState } from "@/waddles/directory";
import type { DiscoveredExtensionCommand, ExtensionCommandAction, ExtensionCommandResult } from "@/lib/xmpp/extension-commands";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import type { MessageThreadIndex } from "@/channels/threads";
import { formatTimelineStamp, formatTimelineDayDivider, isSameTimelineDay } from "@/channels/timeline";
import { useExtensionLauncher } from "@/channels/extension-launcher";
import { useJumpToLiveEdge } from "@/ui/use-jump-to-live-edge";
import { ArrowDown, ArrowUp } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import ChatHeader, { type ChannelHeaderMember } from "@/components/chat/ChatHeader.vue";
import CallExpandedSurface from "@/components/calls/CallExpandedSurface.vue";
import ConversationCallBanner from "@/components/calls/ConversationCallBanner.vue";
import CallSplitContainer from "@/components/calls/CallSplitContainer.vue";
import ExtensionPalette from "@/components/chat/ExtensionPalette.vue";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import UserProfileDrawer from "@/components/chat/UserProfileDrawer.vue";
import VirtualTimeline from "@/components/chat/VirtualTimeline.vue";
import Skeleton from "@/components/ui/Skeleton.vue";

type MessageSkeletonRow = {
  id: number;
  grouped: boolean;
  authorWidth: string;
  lines: string[];
};

const skeletonMessageRows: ReadonlyArray<MessageSkeletonRow> = Object.freeze([
  { id: 1, grouped: false, authorWidth: "5rem",   lines: ["72%"] },
  { id: 2, grouped: true,  authorWidth: "",       lines: ["55%", "38%"] },
  { id: 3, grouped: false, authorWidth: "6.5rem", lines: ["88%", "63%", "41%"] },
  { id: 4, grouped: false, authorWidth: "4.5rem", lines: ["49%"] },
  { id: 5, grouped: true,  authorWidth: "",       lines: ["66%"] },
  { id: 6, grouped: false, authorWidth: "5.75rem", lines: ["78%", "52%"] },
]);

const draft = defineModel<string>("draft", { required: true });
const forumTitle = defineModel<string>("forumTitle", { default: "" });
const pinnedPanelOpen = defineModel<boolean>("pinnedPanelOpen", { default: false });

const props = defineProps<{
  waddle: SpaceSummary | null;
  channel: ChannelSummary | null;
  roomJid?: string | null;
  dmPeer?: { peerJid: string; peerUsername: string; presenceShow?: string } | null;
  sidebarMode?: "channels" | "dms";
  messages: TimelineMessage[];
  firstUnseenId: string | null;
  xmppStatus: XmppStatusSnapshot;
  actionError: string;
  errorActionLabel?: string | null;
  updateAvailable: boolean;
  isApplyingUpdate: boolean;
  isLoadingMessages: boolean;
  isLoadingOlderMessages: boolean;
  hasOlderMessages: boolean;
  isSending: boolean;
  canManageChannels: boolean;
  memberCount: number | null;
  memberState: MemberLoadState;
  typingUsers: string[];
  currentUser?: string;
  currentUserJid?: string;
  selfDomain?: string;
  avatarUrlByAuthor: Record<string, string | null>;
  authorJidByNick?: Record<string, string>;
  giphyApiKey: string;
  mentionCandidates: MentionCandidate[];
  roomHats: RoomHats;
  roomAuthority: RoomAuthority;
  roomPresence: RoomPresence;
  roomLastSeen: Record<string, number>;
  slowModeCooldown: number;
  searchResults: MessageSearchResult[];
  isSearching: boolean;
  uploadProgress: { uploading: boolean; progress: number; filename: string };
  threadIndex: MessageThreadIndex;
  xmppClient?: BrowserXmppClient | null;
  notifySettings: NotifySettingsStore;
  reactionMode?: { selectedMessageId: string | null } | null;
  ensureMessageLoaded?: (messageId: string) => Promise<boolean>;
  invokeExtensionAction?: (action: ExtensionAnnotationAction) => Promise<ExtensionCommandResult>;
  sendPublicChannelMessage?: (body: string) => Promise<void>;
}>();

const emit = defineEmits<{
  send: [
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files?: Array<File | Blob>,
    replyTo?: { id: string; author: string; body?: string },
    forumTitle?: string,
  ];
  typing: [];
  editMessage: [messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[]];
  retractMessage: [messageId: string];
  reactMessage: [messageId: string, emoji: string];
  editChannel: [];
  openNav: [];
  openDetails: [];
  search: [query: string];
  clearSearch: [];
  openDm: [peerJid: string];
  openThread: [threadId: string, targetMessageId?: string];
  joinChannelCall: [channelId: string | null, roomJid: string, media: CallMedia];
  answerDm: [peerJid: string, remoteFullJid: string, sid: string, media: CallMedia];
  reconnectDm: [peerJid: string, media: CallMedia];
  refreshUpdate: [];
  loadOlder: [];
  retryLoad: [];
  pinMessage: [messageId: string];
  unpinMessage: [messageId: string];
}>();

// Hide thread members from the main feed — they live inside the thread panel.
// Keep thread roots (id === threadId) and any message with no threadId.
const feedMessages = computed(() =>
  props.messages.filter((m) => !m.threadId || m.id === m.threadId),
);
const { mode: scrollDirection, isTopPinned } = useScrollDirectionPreference();
const orderedFeedMessages = computed(() =>
  orderTimelineForScrollDirection(feedMessages.value, scrollDirection.value),
);

// Materialise the room's current occupant roster for the channel
// header's live presence stack. Keys of `roomPresence` are the
// authoritative nick set (XEP-0045 occupant tracking), so we walk
// that and join in avatar URLs + real JIDs from the per-author
// indices the timeline already maintains. No new server calls.
// The current user is excluded — the stack answers "who else is
// here right now?", which is the question worth answering at a
// glance. The members panel still shows the full roster.
const channelHeaderMembers = computed<ChannelHeaderMember[]>(() => {
  if (!props.channel) return [];
  const presence = props.roomPresence;
  if (!presence) return [];
  const me = props.currentUser;
  const members: ChannelHeaderMember[] = [];
  for (const [nick, p] of Object.entries(presence)) {
    if (me && nick === me) continue;
    members.push({
      nick,
      jid: props.authorJidByNick?.[nick],
      avatarUrl: props.avatarUrlByAuthor[nick] ?? null,
      presence: p,
    });
  }
  return members;
});
const newMessagesDividerPlacement = computed(() =>
  getNewMessagesDividerPlacement(scrollDirection.value),
);
const olderSentinelPosition = computed(() => scrollDirection.value === "social" ? "end" : "start");

const replyingTo = ref<{ id: string; author: string; body?: string; preview?: string } | null>(null);
const autoOpenedThreadIds = new Set<string>();

type MessageComposerHandle = {
  addAttachments: (files: Array<File | Blob>) => void;
  focus: () => void;
  focusExtensions: () => void;
};
type ExtensionPaletteHandle = {
  focus: () => void;
};

function focusComposer() {
  // Wait for the reply chip/state update so the editor focus wins over the
  // message action button that was just clicked.
  void nextTick(() => composerRef.value?.focus());
}

function closeExtensionLauncher() {
  extensionLauncher.close();
}

function beginReply(message: TimelineMessage) {
  const author = message.author;
  replyingTo.value = {
    id: message.id,
    author,
    ...(message.body ? { body: message.body, preview: message.body } : {}),
  };
  focusComposer();
}

function cancelReply() {
  replyingTo.value = null;
}

const replyJumpNotice = ref("");
let replyJumpNoticeTimeout: ReturnType<typeof setTimeout> | null = null;

function clearReplyJumpNotice() {
  if (replyJumpNoticeTimeout) {
    clearTimeout(replyJumpNoticeTimeout);
    replyJumpNoticeTimeout = null;
  }
  replyJumpNotice.value = "";
}

function showReplyJumpNotice(message: string) {
  clearReplyJumpNotice();
  replyJumpNotice.value = message;
  replyJumpNoticeTimeout = setTimeout(() => {
    replyJumpNotice.value = "";
    replyJumpNoticeTimeout = null;
  }, 2800);
}

// Tracks pending flash timers so repeated jumps don't race each other — a
// stale fade/cleanup from the previous animation would otherwise remove the
// classes in the middle of the new flash.
let flashEl: HTMLElement | null = null;
let flashFadeTimer: ReturnType<typeof setTimeout> | null = null;
let flashCleanupTimer: ReturnType<typeof setTimeout> | null = null;

function cancelPendingFlash() {
  if (flashFadeTimer !== null) {
    clearTimeout(flashFadeTimer);
    flashFadeTimer = null;
  }
  if (flashCleanupTimer !== null) {
    clearTimeout(flashCleanupTimer);
    flashCleanupTimer = null;
  }
  if (flashEl) {
    flashEl.classList.remove("message-jump-flash", "message-jump-flash-fade");
    flashEl = null;
  }
}

async function scrollToMessage(messageId: string) {
  if (props.ensureMessageLoaded && !await props.ensureMessageLoaded(messageId)) {
    showReplyJumpNotice(getReplyJumpNotice(false));
    return;
  }
  // VirtualTimeline + the data-message-id index match items by their
  // primary `id` only. Pinned-panel "jump to message" passes a
  // XEP-0359 stanza-id, which on the timeline lives in `wireIds[]`,
  // not `id`. Resolve the candidate to the primary id first so both
  // paths work (#414).
  const resolvedId = findMessageById(props.messages, messageId)?.id ?? messageId;
  if (await virtualTimelineRef.value?.scrollToMessageId(resolvedId, "center")) {
    await nextTick();
  }
  const el = findMessageElementById(messagesContainer.value, resolvedId);
  const notice = getReplyJumpNotice(el instanceof HTMLElement);
  if (!notice && el instanceof HTMLElement) {
    clearReplyJumpNotice();
    cancelPendingFlash();
    el.scrollIntoView({ behavior: "smooth", block: "center" });
    // Force a reflow before re-adding so repeat clicks re-trigger the animation.
    void el.offsetWidth;
    el.classList.add("message-jump-flash");
    flashEl = el;
    flashFadeTimer = setTimeout(() => {
      flashFadeTimer = null;
      el.classList.add("message-jump-flash-fade");
    }, 200);
    flashCleanupTimer = setTimeout(() => {
      flashCleanupTimer = null;
      el.classList.remove("message-jump-flash", "message-jump-flash-fade");
      if (flashEl === el) flashEl = null;
    }, 2000);
    return;
  }

  showReplyJumpNotice(notice);
}

async function openSearchResult(result: MessageSearchResult) {
  if (result.threadId && result.threadId !== result.id) {
    if (props.ensureMessageLoaded && !await props.ensureMessageLoaded(result.id)) {
      showReplyJumpNotice(getReplyJumpNotice(false));
      return;
    }
    emit("openThread", result.threadId, result.id);
    return;
  }
  await scrollToMessage(result.id);
}

function onSend(body: string, markup: MarkupSpan[], references: MessageReference[], files?: Array<File | Blob>) {
  const pending = replyingTo.value;
  emit(
    "send",
    body,
    markup,
    references,
    files,
    pending ? { id: pending.id, author: pending.author, ...(pending.body ? { body: pending.body } : {}) } : undefined,
    !pending && detectForumChannel(props.channel) ? forumTitle.value : undefined,
  );
  // Don't clear replyingTo here — wait for send to complete (success or failure)
}

function onSelectGif(url: string) {
  onSend(url, [], []);
}

const messagesContainer = ref<HTMLDivElement | null>(null);
type VirtualTimelineHandle = ComponentPublicInstance & {
  scrollElement: HTMLDivElement | null;
  scrollToMessageId: (messageId: string, align?: "start" | "center" | "end") => Promise<boolean>;
  scrollToPinnedEdge: (mode: ScrollDirectionMode) => Promise<boolean>;
};
const virtualTimelineRef = ref<VirtualTimelineHandle | null>(null);
const setMessagesContainer = (el: HTMLDivElement | null) => {
  messagesContainer.value = el;
  void nextTick(() => {
    updateCurrentDayMarker();
    jumpToLive.updateDistance();
  });
};
const setVirtualTimelineRef = (
  instance: VirtualTimelineHandle | null,
) => {
  virtualTimelineRef.value = instance;
  setMessagesContainer(instance?.scrollElement ?? null);
};

// "Jump to latest" floating button — shown when the user has scrolled
// away from the live edge. The live-edge anchor flips with the user's
// scroll-direction preference (social = top, chat = bottom), so the
// same button reads "↑ Latest" or "↓ Latest" accordingly.
const jumpToLive = useJumpToLiveEdge({
  scrollElement: computed(() => virtualTimelineRef.value?.scrollElement ?? null),
  mode: scrollDirection,
  scrollToEdge: (mode) => virtualTimelineRef.value?.scrollToPinnedEdge(mode) ?? false,
});

function onMessagesScroll() {
  updateCurrentDayMarker();
  jumpToLive.updateDistance();
}
const currentDayMarkerLabel = ref("");
const composerRef = ref<MessageComposerHandle | null>(null);
const setComposerRef = (instance: MessageComposerHandle | null) => {
  composerRef.value = instance;
};
const extensionPaletteRef = ref<ExtensionPaletteHandle | null>(null);
const setExtensionPaletteRef = (instance: ExtensionPaletteHandle | null) => {
  extensionPaletteRef.value = instance;
};
const extensionLauncher = useExtensionLauncher({
  xmppClient: computed(() => props.xmppClient),
  roomJid: computed(() => props.roomJid),
  invokeExtensionAction: computed(() => props.invokeExtensionAction),
  sendPublicChannelMessage: computed(() => props.sendPublicChannelMessage),
  focusPalette: () => extensionPaletteRef.value?.focus(),
  focusComposerExtensions: () => composerRef.value?.focusExtensions(),
});
const extensionLauncherOpen = extensionLauncher.open;
const extensionLauncherState = extensionLauncher.state;
const extensionLauncherDetail = extensionLauncher.detail;
const allDiscoveredCommands = extensionLauncher.commands;
const availableExtensionCommands = extensionLauncher.availableCommands;
const extensionCommandStates = extensionLauncher.commandStates;
const extensionCommandForms = extensionLauncher.commandForms;
const extensionCommandActions = extensionLauncher.commandActions;
const openExtensionLauncher = extensionLauncher.toggle;
const invokeExtensionCommand = extensionLauncher.invokeCommand;
const updateExtensionCommandField = extensionLauncher.updateField;
const resetExtensionLauncherState = extensionLauncher.reset;
const submitExtensionCommandForm = extensionLauncher.submitForm;
const invokeCommandResultAction = extensionLauncher.invokeResultAction;
async function dispatchSlashCommand(
  invocation: Parameters<typeof extensionLauncher.dispatchSlashInvocation>[0],
): Promise<boolean> {
  const ok = await extensionLauncher.dispatchSlashInvocation(invocation);
  if (ok) {
    draft.value = "";
    if (isForumChannel.value) forumTitle.value = "";
  }
  return ok;
}
const inMucContext = computed(() => !!props.roomJid);
const callRoomJid = computed(() => props.roomJid ?? props.channel?.jid ?? null);

watch(
  () => props.xmppClient,
  (client) => {
    if (client) void extensionLauncher.ensureDiscovered();
  },
  { immediate: true },
);
const showSearch = ref(false);
const searchInput = ref("");
const searchSubmitted = ref(false);
const searchInputEl = ref<HTMLInputElement | null>(null);

// Drop the caret straight into the search input when the bar opens —
// without this the user had to click the input a second time after
// clicking the header Search button. Matches the autofocus convention
// the GIF picker already follows.
watch(showSearch, async (open) => {
  if (!open) return;
  await nextTick();
  searchInputEl.value?.focus();
});
const avatarUrlByAuthor = computed(() => props.avatarUrlByAuthor ?? {});
const isForumChannel = computed(() => detectForumChannel(props.channel));

// #414: pin state for the current room. Hydrated by the controller.
// Reads from the derived `$pinnedStanzaIds` map (roomJid → Set<stanzaId>)
// matching the #414 PRD contract for a presence-check store.
const pinnedStanzaIdsByRoom = useStore($pinnedStanzaIds);
const pinnedStanzaIdsForRoom = computed(() => {
  if (!props.roomJid) return null;
  return pinnedStanzaIdsByRoom.value.get(props.roomJid) ?? null;
});
function isPinnedMessage(msg: TimelineMessage): boolean {
  const set = pinnedStanzaIdsForRoom.value;
  if (!set) return false;
  if (msg.id && set.has(msg.id)) return true;
  // Also match wireIds (XEP-0359 stanza-id mirrors).
  const wireIds = (msg as TimelineMessage & { wireIds?: string[] }).wireIds;
  return Boolean(wireIds?.some((wid) => set.has(wid)));
}
// #415: pin gate combines the room's pin_permission with the current
// user's MUC affiliation (XEP-0045 §5.2 — the persistent authority
// channel, not XEP-0317 hats, which are descriptive and confer no
// authority). AdminsOnly admits owners and admins; Anyone admits
// any joined member. Membership uses `roomPresence` because it
// tracks every joined occupant regardless of authority — a present
// `currentUser` + a known channel is the canonical "we are in this
// room" signal.
//
// The action-sheet updates the instant the local user toggles the
// policy via the edit dialog; owner-config changes from another
// client only flow in on the next topology refresh until the
// server emits status-code-104 on owner-config set (separate
// follow-up).
const currentUserCanPin = computed(() => {
  const me = props.currentUser;
  if (!me) return false;
  if (props.roomPresence[me] === undefined) return false;
  const channelPolicy = props.channel?.pinPermission ?? "admins-only";
  if (channelPolicy === "anyone") return true;
  const myAffiliation = props.roomAuthority[me]?.affiliation ?? "none";
  return myAffiliation === "owner" || myAffiliation === "admin";
});
const canShowComposer = computed(() => !!(props.channel || props.dmPeer));
const queuedMessageCount = computed(() =>
  props.messages.filter((message) =>
    message.isSelf && (message.deliveryStatus === "sending" || message.deliveryStatus === "failed")
  ).length,
);
const popoverAuthor = ref<{ username: string; jid: string } | null>(null);
const profileDrawerOpen = computed({
  get: () => !!popoverAuthor.value,
  set: (v: boolean) => { if (!v) popoverAuthor.value = null; },
});
const conversationScope = computed(() => [
  props.sidebarMode ?? "channels",
  props.waddle?.id ?? "",
  props.channel?.id ?? "",
  props.dmPeer?.peerJid ?? "",
].join(":"));

watch(conversationScope, () => {
  autoOpenedThreadIds.clear();
  showSearch.value = false;
  searchInput.value = "";
  searchSubmitted.value = false;
  emit("clearSearch");
});

watch(() => props.messages, (newMessages, oldMessages) => {
  // On the initial load (messages transition from 0 → N, e.g. MAM backfill),
  // suppress auto-open for all threads already present in the archive so we
  // don't pop open a thread panel every time the user enters a room.
  if (!oldMessages || oldMessages.length === 0) {
    const byId = new Map(newMessages.map((m) => [m.id, m]));
    for (const msg of newMessages) {
      if (!msg.threadId || msg.threadId === msg.id) continue;
      if (autoOpenedThreadIds.has(msg.threadId)) continue;
      if (byId.get(msg.threadId)?.isSelf) autoOpenedThreadIds.add(msg.threadId);
    }
    return;
  }
  // Don't auto-open when the user is paging backward through older history —
  // those messages have older timestamps and are not live replies.
  if (props.isLoadingOlderMessages || props.activeThread) return;
  const maxOldCreatedAt = oldMessages.reduce(
    (max, m) => (m.createdAt > max ? m.createdAt : max),
    "",
  );
  const prevIds = new Set(oldMessages.map((m) => m.id));
  const incoming = newMessages.filter(
    (m) => !prevIds.has(m.id) && m.createdAt > maxOldCreatedAt,
  );
  const threadId = findThreadToAutoOpen(incoming, newMessages, autoOpenedThreadIds);
  if (threadId) {
    autoOpenedThreadIds.add(threadId);
    emit("openThread", threadId);
  }
});
const hasSeenOnline = ref(props.xmppStatus.state === "online");
const showReconnectedNotice = ref(false);
let reconnectedNoticeTimeout: ReturnType<typeof setTimeout> | null = null;

function clearReconnectedNotice() {
  if (reconnectedNoticeTimeout) {
    clearTimeout(reconnectedNoticeTimeout);
    reconnectedNoticeTimeout = null;
  }
  showReconnectedNotice.value = false;
}

watch(() => props.xmppStatus.state, (state, previousState) => {
  if (state === "online") {
    if (previousState && previousState !== "online" && hasSeenOnline.value) {
      clearReconnectedNotice();
      showReconnectedNotice.value = true;
      reconnectedNoticeTimeout = setTimeout(() => {
        showReconnectedNotice.value = false;
        reconnectedNoticeTimeout = null;
      }, 4200);
    }
    hasSeenOnline.value = true;
    return;
  }

  clearReconnectedNotice();
});

const connectionNotice = computed(() =>
  getConnectionNoticeCopy({
    status: props.xmppStatus,
    queuedMessageCount: queuedMessageCount.value,
    showReconnected: showReconnectedNotice.value,
  }),
);
const connectionStatusIcon = computed(() => {
  switch (connectionNotice.value?.tone) {
    case "offline":
      return WifiOff;
    case "reconnecting":
      return RefreshCw;
    case "error":
      return AlertCircle;
    case "reconnected":
      return CheckCircle2;
    default:
      return WifiOff;
  }
});
const connectionStatusClasses = computed(() => {
  switch (connectionNotice.value?.tone) {
    case "offline":
      // Passive state — user already knows they're disconnected.
      // Keep the chip quiet: muted bg tint, no glow halo.
      return {
        banner: "bg-muted/35 text-foreground",
        iconWrap: "border-border/70 bg-background/60 text-muted-foreground/80",
        chip: "border-border/70 bg-muted/25 text-muted-foreground/80",
        body: "text-muted-foreground",
      };
    case "reconnecting":
      // Active reconnection — warning-tinted bg + warning-glow halo
      // reach the eye without becoming an alarm. The icon's spinner
      // carries the "moving" signal.
      return {
        banner: "bg-warning/10 text-foreground",
        iconWrap: "border-warning/15 bg-background/60 text-warning/80",
        chip: "chat-connection-chip-glow--warning border-warning/35 bg-warning/10 text-warning/90",
        body: "text-foreground/75",
      };
    case "error":
      // Wants user attention (session expired, etc.). Destructive-
      // tinted bg + destructive-glow halo distinguish it from the
      // reconnecting tone without escalating to a banner.
      return {
        banner: "bg-destructive/10 text-foreground",
        iconWrap: "border-destructive/15 bg-background/60 text-destructive/80",
        chip: "chat-connection-chip-glow--destructive border-destructive/35 bg-destructive/10 text-destructive/90",
        body: "text-foreground/80",
      };
    case "reconnected":
      // Brief celebration. Primary-tinted bg + --glow-strong halo
      // so the chip reads "we're back" before the banner ages out.
      return {
        banner: "bg-primary/8 text-foreground",
        iconWrap: "border-primary/12 bg-background/60 text-primary/80",
        chip: "chat-connection-chip-glow--primary border-primary/35 bg-primary/10 text-primary/90",
        body: "text-foreground/75",
      };
    default:
      return null;
  }
});
const updateNoticeBody = computed(() =>
  props.isApplyingUpdate
    ? "Refreshing to load the latest version."
    : "A newer version is ready. Refresh to load it.",
);

async function scrollToPinnedEdge(mode: ScrollDirectionMode) {
  if (await virtualTimelineRef.value?.scrollToPinnedEdge(mode)) return true;
  await nextTick();
  const el = messagesContainer.value;
  if (!el) return false;
  el.scrollTop = getPinnedScrollTop(el, mode);
  return true;
}

defineExpose({ messagesContainer, scrollToPinnedEdge, scrollToMessage });

function doSearch() {
  searchSubmitted.value = !!searchInput.value.trim();
  emit("search", searchInput.value);
}

function closeSearch() {
  showSearch.value = false;
  searchInput.value = "";
  searchSubmitted.value = false;
  emit("clearSearch");
}

function presenceText(show?: string): string {
  if (show === "available") return "online";
  if (show === "away") return "away";
  if (show === "dnd") return "do not disturb";
  if (show === "xa") return "extended away";
  return "offline";
}

function presenceTextForAuthor(username: string): string {
  if (props.dmPeer?.peerUsername === username) return presenceText(props.dmPeer?.presenceShow);
  const roomShow = props.roomPresence[username];
  if (roomShow) return presenceText(roomShow === "online" ? "available" : roomShow);
  return "offline";
}

function onAvatarClick(author: string) {
  const authorJid = props.authorJidByNick?.[author]
    ?? (props.selfDomain ? `${author}@${props.selfDomain}` : null);
  if (!authorJid) return;
  popoverAuthor.value = { username: author, jid: authorJid };
}

function openPopoverDm() {
  if (!popoverAuthor.value) return;
  emit("openDm", popoverAuthor.value.jid);
  popoverAuthor.value = null;
}

// -- Drag-and-drop file upload --
const isDragging = ref(false);
let dragLeaveTimeout: ReturnType<typeof setTimeout> | null = null;

function onDragEnter(e: DragEvent) {
  e.preventDefault();
  if (!e.dataTransfer?.types.includes("Files")) return;
  if (dragLeaveTimeout) { clearTimeout(dragLeaveTimeout); dragLeaveTimeout = null; }
  isDragging.value = true;
}

function onDragOver(e: DragEvent) {
  e.preventDefault();
}

function onDragLeave() {
  dragLeaveTimeout = setTimeout(() => { isDragging.value = false; }, 50);
}

function onDrop(e: DragEvent) {
  e.preventDefault();
  isDragging.value = false;
  const files = extractFilesFromEvent(e);
  if (files.length > 0) composerRef.value?.addAttachments(files);
}

watch(conversationScope, () => {
  cancelReply();
  clearReplyJumpNotice();
  clearReconnectedNotice();
  resetExtensionLauncherState();
  forumTitle.value = "";
});

// Clear reply context only on successful send; preserve on failure so user can retry
watch(() => props.isSending, (sending, prevSending) => {
  if (prevSending && !sending && replyingTo.value) {
    // Send completed: clear reply context only if no error occurred
    if (!props.actionError) {
      replyingTo.value = null;
    }
    // On error, keep reply context so user can retry without re-selecting
  }
});

onBeforeUnmount(() => {
  clearReconnectedNotice();
  if (replyJumpNoticeTimeout) {
    clearTimeout(replyJumpNoticeTimeout);
  }
});

function showDividerBefore(messageId: string): boolean {
  return props.firstUnseenId === messageId && newMessagesDividerPlacement.value === "before";
}

function showDividerAfter(messageId: string): boolean {
  return props.firstUnseenId === messageId && newMessagesDividerPlacement.value === "after";
}

function updateCurrentDayMarker() {
  const container = messagesContainer.value;
  if (!container) {
    currentDayMarkerLabel.value = "";
    return;
  }

  const markerEls = [
    ...container.querySelectorAll<HTMLElement>("[data-day-marker-created-at], [data-message-created-at]"),
  ];
  if (markerEls.length === 0) {
    currentDayMarkerLabel.value = "";
    return;
  }

  const containerTop = container.getBoundingClientRect().top;
  const probeTop = containerTop + 1;
  let current = markerEls[0];
  for (const el of markerEls) {
    const rect = el.getBoundingClientRect();
    if (rect.bottom < probeTop) {
      current = el;
      continue;
    }
    if (rect.top <= probeTop || current === markerEls[0]) {
      current = el;
    }
    break;
  }

  const createdAt = current.dataset.dayMarkerCreatedAt ?? current.dataset.messageCreatedAt;
  currentDayMarkerLabel.value = createdAt ? formatTimelineDayDivider(createdAt) : "";
}

watch(
  [orderedFeedMessages, () => props.isLoadingMessages, conversationScope],
  () => {
    void nextTick(updateCurrentDayMarker);
  },
  { flush: "post" },
);

// Burst window: same author + < 5 min apart + same day, with no
// intervening other-author message in the rendered order.
const BURST_WINDOW_MS = 5 * 60 * 1000;

const messageDisplayMeta = computed(() => {
  const grouped = new Set<string>();
  const dayDivider = new Set<string>();
  const list = orderedFeedMessages.value;
  for (let i = 0; i < list.length; i++) {
    const cur = list[i];
    if (!cur) continue;
    const prev = i > 0 ? list[i - 1] : null;
    if (!prev) continue;
    const sameDay = isSameTimelineDay(prev.createdAt, cur.createdAt);
    if (!sameDay) dayDivider.add(cur.id);
    if (
      sameDay
      && prev.author === cur.author
      && Math.abs(new Date(cur.createdAt).getTime() - new Date(prev.createdAt).getTime()) < BURST_WINDOW_MS
    ) {
      grouped.add(cur.id);
    }
  }
  return { grouped, dayDivider };
});

function isGroupedFollowUp(messageId: string): boolean {
  return messageDisplayMeta.value.grouped.has(messageId);
}

const THREAD_CHIP_MAX_PARTICIPANTS = 5;

interface ThreadChipParticipant {
  nick: string;
  avatarUrl?: string | null;
  presence: import("@/lib/xmpp-client").OccupantPresence;
}

// Distinct participants for a thread chip: walk newest→oldest, dedup,
// keep EVERYONE who replied — including the thread author themselves
// and the current user. Previously the current user was excluded
// (logic: "the chip answers who *else* has been talking"), but that
// made the chip read inconsistently: a stranger's reply showed an
// avatar, my own reply showed nothing. Capped at
// THREAD_CHIP_MAX_PARTICIPANTS — MessageCard renders only the first
// N visibly and shows a "+N" overflow chip.
function threadChipParticipants(messageId: string): ThreadChipParticipant[] {
  const entry = props.threadIndex.get(messageId);
  if (!entry) return [];
  const seen = new Set<string>();
  const ordered: ThreadChipParticipant[] = [];
  const children = entry.directChildren;
  for (let i = children.length - 1; i >= 0; i--) {
    const c = children[i];
    if (!c) continue;
    if (seen.has(c.author)) continue;
    seen.add(c.author);
    ordered.push({
      nick: c.author,
      avatarUrl: props.avatarUrlByAuthor[c.author] ?? null,
      presence: props.roomPresence[c.author] ?? "offline",
    });
    if (ordered.length >= THREAD_CHIP_MAX_PARTICIPANTS) break;
  }
  return ordered;
}

function threadChipLastReplyAt(messageId: string): string | undefined {
  const entry = props.threadIndex.get(messageId);
  return entry?.directChildren.at(-1)?.createdAt;
}

function showDayDividerBefore(messageId: string): boolean {
  return messageDisplayMeta.value.dayDivider.has(messageId);
}

function dayDividerLabel(createdAt: string): string {
  return formatTimelineDayDivider(createdAt);
}
</script>

<template>
  <div
    class="flex-1 flex flex-col min-w-0 min-h-0 bg-background relative"
    @dragenter="onDragEnter"
    @dragover="onDragOver"
    @dragleave="onDragLeave"
    @drop="onDrop"
  >
    <!-- Drop zone overlay -->
    <div
      v-if="isDragging"
      class="z-popover absolute inset-3 flex flex-col items-center justify-center gap-2 rounded-lg border-2 border-dashed border-primary bg-primary/10 pointer-events-none animate-fade-in"
    >
      <Upload class="w-8 h-8 text-primary" />
      <span class="type-display-title text-primary">Drop files to upload</span>
    </div>

    <ChatHeader
      v-model:show-search="showSearch"
      v-model:show-pinned-panel="pinnedPanelOpen"
      :waddle="waddle"
      :channel="channel"
      :dm-peer="dmPeer"
      :call-room-jid="callRoomJid"
      :is-forum-channel="isForumChannel"
      :can-manage-channels="canManageChannels"
      :member-count="memberCount"
      :member-state="memberState"
      :members="channelHeaderMembers"
      :connection-notice="connectionNotice"
      :connection-status-classes="connectionStatusClasses"
      :connection-status-icon="connectionStatusIcon"
      :xmpp-client="xmppClient"
      :notify-settings="notifySettings"
      :show-call-active-pill="false"
      :show-dm-call-activity-controls="false"
      :hide-muc-start-controls-when-active-call="true"
      @open-nav="emit('openNav')"
      @open-details="emit('openDetails')"
      @edit-channel="emit('editChannel')"
      @select-member="(jid: string) => emit('openDm', jid)"
    />
    <ConversationCallBanner
      :room-jid="callRoomJid"
      :channel-id="channel?.id ?? null"
      :channel-name="channel?.name ?? null"
      :dm-peer-jid="dmPeer?.peerJid ?? null"
      :dm-peer-name="dmPeer?.peerUsername ?? null"
      @join-channel-call="(channelId, roomJid, media) => emit('joinChannelCall', channelId, roomJid, media)"
      @answer-dm="(peerJid, remoteFullJid, sid, media) => emit('answerDm', peerJid, remoteFullJid, sid, media)"
      @reconnect-dm="(peerJid, media) => emit('reconnectDm', peerJid, media)"
    />
    <div
      v-if="connectionNotice && connectionStatusClasses"
      role="status"
      aria-live="polite"
      aria-atomic="true"
      class="border-b border-border/80 animate-fade-in"
      :class="connectionStatusClasses.banner"
    >
      <div class="chat-message-lane flex items-start gap-3 px-[var(--chat-content-inline)] py-3">
        <div
          class="mt-0.5 flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full border"
          :class="connectionStatusClasses.iconWrap"
        >
          <component
            :is="connectionStatusIcon"
            class="h-4 w-4"
            :class="{ 'motion-safe:animate-spin': connectionNotice.tone === 'reconnecting' }"
          />
        </div>
        <div class="flex min-w-0 flex-col gap-0.5">
          <p class="type-control">
            {{ connectionNotice.title }}
          </p>
          <p class="type-caption chat-copy-measure" :class="connectionStatusClasses.body">
            {{ connectionNotice.body }}
          </p>
        </div>
      </div>
    </div>
    <div
      v-if="updateAvailable"
      role="status"
      aria-live="polite"
      aria-atomic="true"
      class="border-b border-primary/12 bg-primary/8 text-foreground"
    >
      <div class="chat-message-lane flex flex-col gap-3 px-[var(--chat-content-inline)] py-3.5 sm:flex-row sm:items-center sm:justify-between">
        <div class="flex min-w-0 items-start gap-3">
          <div class="mt-0.5 flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full border border-primary/15 bg-background/75 text-primary">
            <RefreshCw
              class="h-4 w-4"
              :class="{ 'motion-safe:animate-spin': isApplyingUpdate }"
            />
          </div>
          <div class="flex min-w-0 flex-col gap-0.5">
            <p class="type-control">
              Update ready
            </p>
            <p class="type-caption chat-copy-measure text-foreground/75">
              {{ updateNoticeBody }}
            </p>
          </div>
        </div>
        <button
          type="button"
          class="type-control inline-flex h-8 shrink-0 items-center justify-center gap-1.5 rounded-full bg-primary px-3.5 text-primary-foreground transition-colors hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/35 disabled:cursor-wait disabled:opacity-75"
          :disabled="isApplyingUpdate"
          @click="emit('refreshUpdate')"
        >
          <RefreshCw
            class="h-3.5 w-3.5"
            :class="{ 'motion-safe:animate-spin': isApplyingUpdate }"
          />
          <span>{{ isApplyingUpdate ? "Refreshing…" : "Refresh" }}</span>
        </button>
      </div>
    </div>

    <!-- Search bar -->
    <div v-if="showSearch" class="px-[var(--chat-content-inline)] py-2.5 border-b border-border glass-surface flex items-center gap-3 flex-shrink-0 animate-fade-in">
      <div class="chat-message-lane flex items-center gap-3">
        <Search class="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
        <input
          ref="searchInputEl"
          v-model="searchInput"
          placeholder="Search messages…"
          aria-label="Search messages"
          class="type-field flex-1 bg-transparent focus:outline-none placeholder:text-muted-foreground/40"
          @keydown.enter="doSearch"
          @keydown.escape="closeSearch"
        />
        <button
          v-if="searchInput"
          class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground"
          aria-label="Clear search"
          type="button"
          @click="closeSearch"
        >
          <X class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>

    <!-- Search results -->
    <div v-if="showSearch && (searchSubmitted || isSearching)" class="border-b border-border glass-surface max-h-56 overflow-auto flex-shrink-0">
      <div class="chat-message-lane">
        <!-- Loading state — Loader2 spinner alongside the copy so the
             user sees the request is in flight, not just frozen. -->
        <div
          v-if="isSearching"
          class="type-caption flex items-center justify-center gap-2 px-[var(--chat-content-inline)] py-5 text-muted-foreground"
        >
          <Loader2 class="h-3.5 w-3.5 motion-safe:animate-spin" aria-hidden="true" />
          <span>Searching…</span>
        </div>
        <!-- Empty state — SearchX glyph stacked above the copy so the
             "no matches" outcome reads as an authored state, not a
             one-line ghost. -->
        <div
          v-else-if="searchResults.length === 0"
          class="flex flex-col items-center justify-center gap-1.5 px-[var(--chat-content-inline)] py-6 text-center"
        >
          <SearchX class="h-5 w-5 text-muted-foreground/60" aria-hidden="true" />
          <span class="type-caption text-muted-foreground">No matching messages</span>
        </div>
        <div v-else class="divide-y divide-border">
          <!-- Each search result row gets the author's avatar on the
               left so the eye can triage results by who-said-it at a
               glance — the same recognition cue the timeline uses.
               Avatar (xs) + content stack (name + time on row one,
               truncated body on row two) lets the row read as a
               compact preview of the matching message. -->
          <button
            v-for="result in searchResults"
            :key="result.id"
            class="flex w-full items-start gap-3 px-[var(--chat-content-inline)] py-3 text-left hover:bg-muted/50 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/25"
            type="button"
            @click="openSearchResult(result)"
          >
            <AppAvatar
              :name="result.nick"
              :src="avatarUrlByAuthor[result.nick] ?? null"
              :presence="roomPresence[result.nick] ?? 'offline'"
              size="xs"
            />
            <div class="min-w-0 flex-1">
              <div class="flex items-baseline gap-2">
                <span class="type-control">{{ result.nick }}</span>
                <span class="type-meta type-numeric text-muted-foreground">{{ formatTimelineStamp(result.createdAt) }}</span>
              </div>
              <p class="type-caption truncate text-muted-foreground">{{ result.body }}</p>
            </div>
          </button>
        </div>
      </div>
    </div>

    <!-- Error banner -->
    <div
      v-if="actionError"
      class="type-control bg-destructive/10 border-b border-destructive/20 text-destructive animate-fade-in"
      role="alert"
      aria-live="assertive"
      aria-atomic="true"
    >
      <div class="chat-message-lane flex flex-col gap-2 px-[var(--chat-content-inline)] py-3 sm:flex-row sm:items-center sm:justify-between">
        <span>{{ actionError }}</span>
        <button
          v-if="errorActionLabel"
          type="button"
          class="inline-flex h-8 shrink-0 items-center justify-center gap-1.5 rounded-md border border-destructive/25 bg-background/80 px-3 text-destructive transition-colors hover:bg-destructive/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-destructive/30"
          @click="emit('retryLoad')"
        >
          <RefreshCw class="h-3.5 w-3.5" />
          {{ errorActionLabel }}
        </button>
      </div>
    </div>
    <div
      v-if="replyJumpNotice"
      role="status"
      aria-live="polite"
      aria-atomic="true"
      class="type-caption text-muted-foreground bg-muted/35 border-b border-border animate-fade-in"
    >
      <div class="chat-message-lane px-[var(--chat-content-inline)] py-2.5">
        {{ replyJumpNotice }}
      </div>
    </div>

    <!-- Inline split-view for an active call in this conversation.
         MUC and DM ownership gates live inside the component. -->
    <CallSplitContainer
      v-if="callRoomJid || dmPeer?.peerJid"
      :room-jid="callRoomJid ?? undefined"
      :dm-peer-jid="dmPeer?.peerJid"
      :dm-peer-name="dmPeer?.peerUsername"
    />

    <!-- Composer (social / top-pinned mode) -->
    <!-- Note: the composer and typing indicator appear in two spots in the DOM
         so they can be rendered above or below the messages container depending
         on the scroll-direction mode. -->
    <MessageComposer
      v-if="canShowComposer && isTopPinned"
      :ref="setComposerRef"
      v-model:draft="draft"
      v-model:forum-title="forumTitle"
      :channel-name="dmPeer ? dmPeer.peerUsername : (channel?.name ?? 'conversation')"
      :is-forum-channel="isForumChannel"
      :is-sending="isSending"
      :disabled="!canShowComposer"
      :giphy-api-key="giphyApiKey"
      :mention-candidates="mentionCandidates"
      :slow-mode-cooldown="slowModeCooldown"
      :upload-progress="uploadProgress"
      :replying-to="replyingTo"
      :is-top-pinned="true"
      :extensions-open="extensionLauncherOpen"
      :slash-commands="allDiscoveredCommands"
      :in-muc="inMucContext"
      :dispatch-slash-command="dispatchSlashCommand"
      @send="onSend"
      @cancel-reply="cancelReply"
      @typing="emit('typing')"
      @select-gif="onSelectGif"
      @open-extensions="openExtensionLauncher"
    />

    <ExtensionPalette
      v-if="extensionLauncherOpen && isTopPinned"
      :ref="setExtensionPaletteRef"
      :state="extensionLauncherState"
      :detail="extensionLauncherDetail"
      :commands="availableExtensionCommands"
      :command-states="extensionCommandStates"
      :command-forms="extensionCommandForms"
      :command-actions="extensionCommandActions"
      :is-top-pinned="true"
      @close="closeExtensionLauncher"
      @invoke-command="invokeExtensionCommand"
      @submit-form="submitExtensionCommandForm"
      @invoke-action="invokeCommandResultAction"
      @update-field="updateExtensionCommandField"
    />

    <!-- Typing indicator (social / top-pinned mode) -->
    <div
      v-if="typingUsers.length > 0 && isTopPinned"
      class="chat-typing-indicator chat-typing-indicator--social flex-shrink-0 border-b border-border"
    >
      <div class="chat-message-lane chat-typing-indicator__lane">
        <span class="chat-typing-indicator__avatars" aria-hidden="true">
          <span
            v-for="nick in typingUsers.slice(0, 3)"
            :key="`typing-avatar:${nick}`"
            class="chat-typing-indicator__avatar-wrap"
          >
            <AppAvatar
              :name="nick"
              :src="avatarUrlByAuthor[nick] ?? null"
              :presence="roomPresence[nick] ?? 'offline'"
              size="xs"
            />
          </span>
        </span>
        <span class="chat-typing-indicator__bubble">
          <span class="chat-typing-indicator__wave" aria-hidden="true">
            <span class="chat-typing-indicator__wave-dot" />
            <span class="chat-typing-indicator__wave-dot" />
            <span class="chat-typing-indicator__wave-dot" />
          </span>
          <span class="chat-typing-indicator__text">
            <template v-if="typingUsers.length === 1">{{ typingUsers[0] }} is typing</template>
            <template v-else-if="typingUsers.length === 2">{{ typingUsers[0] }} and {{ typingUsers[1] }} are typing</template>
            <template v-else>{{ typingUsers[0] }} and {{ typingUsers.length - 1 }} others are typing</template>
          </span>
        </span>
      </div>
    </div>

    <div
      v-if="currentDayMarkerLabel"
      class="chat-current-day-marker type-section-label"
      role="status"
      aria-live="polite"
    >
      <div class="chat-current-day-marker__lane">
        <span class="chat-current-day-marker__label">{{ currentDayMarkerLabel }}</span>
      </div>
    </div>

    <!-- Messages -->
    <div
      v-if="isLoadingMessages || !channel && !dmPeer || feedMessages.length === 0"
      :ref="setMessagesContainer"
      class="chat-pane-scroll chat-message-scroll flex-1 min-h-0 px-[var(--chat-content-inline)]"
      @scroll="updateCurrentDayMarker"
    >
      <div v-if="isLoadingMessages" class="chat-message-lane flex flex-col gap-1 py-6" aria-busy="true" aria-label="Loading messages">
        <div
          v-for="row in skeletonMessageRows"
          :key="`msg-skel-${row.id}`"
          class="chat-message-grid"
          :class="row.grouped ? 'chat-message-grouped' : ''"
        >
          <div class="chat-message-avatar-cell">
            <Skeleton
              v-if="!row.grouped"
              width="var(--chat-message-avatar-size)"
              height="var(--chat-message-avatar-size)"
              radius="var(--radius-md)"
            />
          </div>
          <div class="chat-message-body-stack flex flex-col gap-1.5">
            <div v-if="!row.grouped" class="flex items-center gap-2">
              <Skeleton :width="row.authorWidth" height="0.65rem" />
              <Skeleton width="2.25rem" height="0.55rem" />
            </div>
            <Skeleton
              v-for="(width, i) in row.lines"
              :key="`msg-skel-line-${row.id}-${i}`"
              :width="width"
              height="0.7rem"
            />
          </div>
        </div>
      </div>

      <div v-else-if="!channel && !dmPeer" class="chat-empty-state">
        <div class="chat-empty-state__halo">
          <span class="chat-empty-state__halo-glow" aria-hidden="true" />
          <span class="chat-empty-state__halo-ring chat-empty-state__halo-ring--muted">
            <component :is="sidebarMode === 'dms' ? MessageCircle : isForumChannel ? MessagesSquare : Hash" class="w-6 h-6 text-primary/70" />
          </span>
        </div>
        <div class="chat-field-stack">
          <p class="type-empty-title">
            {{ sidebarMode === "dms"
              ? "Pick a conversation"
              : isForumChannel
                ? "Pick a forum"
                : "Pick a channel" }}
          </p>
          <p class="type-field text-muted-foreground chat-copy-measure">
            {{ sidebarMode === "dms"
              ? "Open one from the sidebar to keep chatting."
              : isForumChannel
                ? "Open one to browse its topics."
                : "Open one from the sidebar to drop into the conversation." }}
          </p>
        </div>
      </div>

      <div v-else-if="errorActionLabel" class="chat-empty-state">
        <div class="w-12 h-12 rounded-lg bg-destructive/10 flex items-center justify-center">
          <AlertCircle class="w-5 h-5 text-destructive" />
        </div>
        <div class="chat-field-stack">
          <p class="type-empty-title">
            Messages are not available right now.
          </p>
          <p class="type-field text-muted-foreground">
            Check the connection and try again.
          </p>
        </div>
      </div>

      <div v-else-if="feedMessages.length === 0" class="chat-empty-state">
        <div v-if="!dmPeer && !isForumChannel" class="chat-empty-state__mascot-wrap" aria-hidden="true">
          <span class="chat-empty-state__mascot-halo" />
          <img class="chat-empty-state__mascot" src="/waddle-logo.svg" alt="" />
        </div>
        <div v-else class="chat-empty-state__halo">
          <span class="chat-empty-state__halo-glow chat-empty-state__halo-glow--primary" aria-hidden="true" />
          <span class="chat-empty-state__halo-ring chat-empty-state__halo-ring--primary">
            <component :is="dmPeer ? MessageCircle : MessagesSquare" class="w-6 h-6 text-primary" />
          </span>
        </div>
        <div class="chat-field-stack">
          <p class="type-empty-title">
            {{ dmPeer
              ? `Just you and @${dmPeer.peerUsername}`
              : isForumChannel
                ? `Welcome to #${channel?.name}`
                : `It's quiet in #${channel?.name}` }}
          </p>
          <p class="type-field text-muted-foreground chat-copy-measure">
            {{ isForumChannel
              ? "Start the first topic with a clear title so people can follow the thread."
              : dmPeer
                ? "Send the first message to get the conversation going."
                : "Be the one who breaks the silence. Drop a hello, share what you're working on, ask the room something interesting." }}
          </p>
        </div>
      </div>

    </div>

    <div v-else class="chat-message-pane">
    <VirtualTimeline
      :ref="setVirtualTimelineRef"
      :items="orderedFeedMessages"
      :has-older="hasOlderMessages"
      :loading-older="isLoadingOlderMessages"
      :sentinel-position="olderSentinelPosition"
      aria-label="Messages"
      @scroll="onMessagesScroll"
      @load-older="emit('loadOlder')"
    >
      <template #item="{ item: msg }">
          <div
            v-if="showDayDividerBefore(msg.id)"
            class="chat-day-divider type-section-label"
            :data-day-marker-created-at="msg.createdAt"
            role="separator"
            :aria-label="dayDividerLabel(msg.createdAt)"
          >
            <div class="chat-day-divider__rule" />
            <span class="chat-day-divider__label">{{ dayDividerLabel(msg.createdAt) }}</span>
            <div class="chat-day-divider__rule" />
          </div>
          <div
            v-if="showDividerBefore(msg.id)"
            class="chat-new-messages-divider type-section-label"
            data-new-messages-divider
            role="separator"
            aria-label="New messages"
          >
            <div class="chat-new-messages-divider__rule" />
            <span class="chat-new-messages-divider__label">
              <span class="chat-new-messages-divider__pulse" aria-hidden="true" />
              New messages
            </span>
            <div class="chat-new-messages-divider__rule" />
          </div>
          <MessageCard
            :message="msg"
            :current-user="props.currentUser"
            :current-user-jid="props.currentUserJid"
            :avatar-url="avatarUrlByAuthor[msg.author] ?? null"
            :hats="roomHats[msg.author] ?? []"
            :authority="roomAuthority[msg.author] ?? null"
            :presence="roomPresence[msg.author] ?? 'offline'"
            :last-seen="roomLastSeen[msg.author]"
            :author-jid="authorJidByNick?.[msg.author]"
            :thread-reply-count="threadIndex.get(msg.id)?.count ?? 0"
            :thread-participants="threadChipParticipants(msg.id)"
            :thread-last-reply-at="threadChipLastReplyAt(msg.id)"
            :grouped="isGroupedFollowUp(msg.id)"
            :reaction-mode-selected="reactionMode?.selectedMessageId === msg.id"
            :invoke-extension-action="props.invokeExtensionAction"
            :is-pinned="isPinnedMessage(msg)"
            :can-pin-messages="currentUserCanPin"
            @edit="(id, body, m, r) => emit('editMessage', id, body, m, r)"
            @retract="(id) => emit('retractMessage', id)"
            @react="(id, emoji) => emit('reactMessage', id, emoji)"
            @reply="beginReply"
            @scroll-to-message="scrollToMessage"
            @avatar-click="onAvatarClick"
            @open-thread="(tid: string) => emit('openThread', tid)"
            @pin="(id: string) => emit('pinMessage', id)"
            @unpin="(id: string) => emit('unpinMessage', id)"
          />
          <div
            v-if="showDividerAfter(msg.id)"
            class="chat-new-messages-divider type-section-label"
            data-new-messages-divider
            role="separator"
            aria-label="New messages"
          >
            <div class="chat-new-messages-divider__rule" />
            <span class="chat-new-messages-divider__label">
              <span class="chat-new-messages-divider__pulse" aria-hidden="true" />
              New messages
            </span>
            <div class="chat-new-messages-divider__rule" />
          </div>
      </template>
    </VirtualTimeline>
    <button
      v-if="jumpToLive.shouldShow.value"
      type="button"
      class="chat-jump-to-live"
      :class="isTopPinned ? 'chat-jump-to-live--top' : 'chat-jump-to-live--bottom'"
      :aria-label="isTopPinned ? 'Jump to latest message' : 'Jump to latest message'"
      title="Jump to latest"
      @click="jumpToLive.jump"
    >
      <ArrowUp v-if="isTopPinned" class="w-3.5 h-3.5" aria-hidden="true" />
      <ArrowDown v-else class="w-3.5 h-3.5" aria-hidden="true" />
      <span>Latest</span>
    </button>
    </div>

    <!-- Typing indicator (chat / bottom-pinned mode) -->
    <div
      v-if="typingUsers.length > 0 && !isTopPinned"
      class="chat-typing-indicator chat-typing-indicator--chat flex-shrink-0"
    >
      <div class="chat-message-lane chat-typing-indicator__lane">
        <span class="chat-typing-indicator__avatars" aria-hidden="true">
          <span
            v-for="nick in typingUsers.slice(0, 3)"
            :key="`typing-avatar-bottom:${nick}`"
            class="chat-typing-indicator__avatar-wrap"
          >
            <AppAvatar
              :name="nick"
              :src="avatarUrlByAuthor[nick] ?? null"
              :presence="roomPresence[nick] ?? 'offline'"
              size="xs"
            />
          </span>
        </span>
        <span class="chat-typing-indicator__bubble">
          <span class="chat-typing-indicator__wave" aria-hidden="true">
            <span class="chat-typing-indicator__wave-dot" />
            <span class="chat-typing-indicator__wave-dot" />
            <span class="chat-typing-indicator__wave-dot" />
          </span>
          <span class="chat-typing-indicator__text">
            <template v-if="typingUsers.length === 1">{{ typingUsers[0] }} is typing</template>
            <template v-else-if="typingUsers.length === 2">{{ typingUsers[0] }} and {{ typingUsers[1] }} are typing</template>
            <template v-else>{{ typingUsers[0] }} and {{ typingUsers.length - 1 }} others are typing</template>
          </span>
        </span>
      </div>
    </div>

    <!-- Composer -->
    <MessageComposer
      v-if="canShowComposer && !isTopPinned"
      :ref="setComposerRef"
      v-model:draft="draft"
      v-model:forum-title="forumTitle"
      :channel-name="dmPeer ? dmPeer.peerUsername : (channel?.name ?? 'conversation')"
      :is-forum-channel="isForumChannel"
      :is-sending="isSending"
      :disabled="!canShowComposer"
      :giphy-api-key="giphyApiKey"
      :mention-candidates="mentionCandidates"
      :slow-mode-cooldown="slowModeCooldown"
      :upload-progress="uploadProgress"
      :replying-to="replyingTo"
      :extensions-open="extensionLauncherOpen"
      :slash-commands="allDiscoveredCommands"
      :in-muc="inMucContext"
      :dispatch-slash-command="dispatchSlashCommand"
      @send="onSend"
      @cancel-reply="cancelReply"
      @typing="emit('typing')"
      @select-gif="onSelectGif"
      @open-extensions="openExtensionLauncher"
    />
    <ExtensionPalette
      v-if="extensionLauncherOpen && !isTopPinned"
      :ref="setExtensionPaletteRef"
      :state="extensionLauncherState"
      :detail="extensionLauncherDetail"
      :commands="availableExtensionCommands"
      :command-states="extensionCommandStates"
      :command-forms="extensionCommandForms"
      :command-actions="extensionCommandActions"
      @close="closeExtensionLauncher"
      @invoke-command="invokeExtensionCommand"
      @submit-form="submitExtensionCommandForm"
      @invoke-action="invokeCommandResultAction"
      @update-field="updateExtensionCommandField"
    />
    <UserProfileDrawer
      v-model:open="profileDrawerOpen"
      :username="popoverAuthor?.username ?? ''"
      :jid="popoverAuthor?.jid ?? ''"
      :avatar-url="popoverAuthor ? avatarUrlByAuthor[popoverAuthor.username] ?? null : null"
      :presence="popoverAuthor ? roomPresence[popoverAuthor.username] : undefined"
      :presence-text="popoverAuthor ? presenceTextForAuthor(popoverAuthor.username) : undefined"
      :hats="popoverAuthor ? roomHats[popoverAuthor.username] : undefined"
      :is-self="popoverAuthor?.username === currentUser"
      :xmpp-client="xmppClient"
      @message="openPopoverDm"
    />
    <!-- Expanded call surface: absolutely positioned over the chat
         content pane (this root has `position: relative`) so the
         surrounding app shell — waddles rail, channel list, thread
         panel — stays visible. Decides its own visibility based on
         call state + ui mode. -->
    <CallExpandedSurface
      v-if="callRoomJid || dmPeer?.peerJid"
      :room-jid="callRoomJid ?? undefined"
      :dm-peer-jid="dmPeer?.peerJid"
      :dm-peer-name="dmPeer?.peerUsername"
    />
  </div>
</template>
