<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch, type ComponentPublicInstance } from "vue";
import { useStore } from "@nanostores/vue";
import { AlertCircle, RefreshCw, Upload } from "lucide-vue-next";
import { $pinnedStanzaIds } from "@/stores/pinned-messages";
import { isForumChannel as detectForumChannel } from "@/lib/channel-types";
import type { NotifySettingsStore } from "@/lib/notify-settings";
import { getReplyJumpNotice } from "@/lib/reply-ux";
import { findThreadToAutoOpen } from "@/lib/thread-auto-open";
import { activeCallChatThreadId } from "@/lib/calls/call-chat-composer";
import { $callState } from "@/lib/calls/call-store";
import { $callDockOpen, $callDockTab } from "@/lib/calls/call-dock-state";
import {
  inboundCallChatThreadIds,
  isCallChatTabFocused,
  syncCallChatUnread,
} from "@/lib/calls/call-chat-unread";
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
import type { ComposerLinkPreviewLookup, ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import type { MessageThreadIndex } from "@/channels/threads";
import { formatTimelineDayDivider, isFeedTimelineMessage } from "@/channels/timeline";
import { useExtensionLauncher } from "@/channels/extension-launcher";
import { useJumpToLiveEdge } from "@/ui/use-jump-to-live-edge";
import { createScrollFrameScheduler } from "@/ui/scroll-frame";
import { ArrowDown, ArrowUp } from "lucide-vue-next";
import ChatHeader, { type ChannelHeaderMember } from "@/components/chat/ChatHeader.vue";
import CallExpandedSurface from "@/components/calls/CallExpandedSurface.vue";
import ConversationCallBanner from "@/components/calls/ConversationCallBanner.vue";
import CallSplitContainer from "@/components/calls/CallSplitContainer.vue";
import ExtensionPalette from "@/components/chat/ExtensionPalette.vue";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import MessageListSkeleton from "@/components/chat/MessageListSkeleton.vue";
import MessageSearchPanel from "@/components/chat/MessageSearchPanel.vue";
import TimelineEmptyState from "@/components/chat/TimelineEmptyState.vue";
import TypingIndicator from "@/components/chat/TypingIndicator.vue";
import UserProfileDrawer from "@/components/chat/UserProfileDrawer.vue";
import VirtualTimeline from "@/components/chat/VirtualTimeline.vue";
import { currentDayMarkerLabelFor } from "./current-day-marker";
import {
  buildMessageDisplayMeta,
  threadChipLastReplyAt as threadChipLastReplyAtFor,
  threadChipParticipants as threadChipParticipantsFor,
} from "./timeline-display-meta";
import { useConnectionNotice } from "./composables/use-connection-notice";
import { useMessageJump } from "./composables/use-message-jump";

const draft = defineModel<string>("draft", { required: true });
const forumTitle = defineModel<string>("forumTitle", { default: "" });
const pinnedPanelOpen = defineModel<boolean>("pinnedPanelOpen", { default: false });

const props = defineProps<{
  waddle: SpaceSummary | null;
  channel: ChannelSummary | null;
  roomJid?: string | null;
  dmPeer?: {
    peerJid: string;
    peerUsername: string;
    presenceShow?: string;
    presenceIdleSince?: number;
  } | null;
  sidebarMode?: "channels" | "dms";
  messages: TimelineMessage[];
  firstUnseenId: string | null;
  xmppStatus: XmppStatusSnapshot;
  actionError: string;
  channelAccessRequired?: boolean;
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
  selfFullJid?: string | null;
  selfDomain?: string;
  avatarUrlByAuthor: Record<string, string | null>;
  authorJidByNick?: Record<string, string>;
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
  sendCallChatMessage?: (
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files: Array<File | Blob> | undefined,
    replyTo: { id: string; author: string; body?: string } | undefined,
    threadOverride: { threadId: string; parentThreadId?: string },
    linkPreview?: ComposerLinkPreviewSendPayload,
  ) => Promise<void>;
}>();

const emit = defineEmits<{
  send: [
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files?: Array<File | Blob>,
    replyTo?: { id: string; author: string; body?: string },
    forumTitle?: string,
    linkPreview?: ComposerLinkPreviewSendPayload,
  ];
  sendCallChat: [
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files: Array<File | Blob> | undefined,
    replyTo: { id: string; author: string; body?: string } | undefined,
    threadOverride: { threadId: string; parentThreadId?: string },
    linkPreview?: ComposerLinkPreviewSendPayload,
  ];
  typing: [];
  editMessage: [messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[], linkPreview?: ComposerLinkPreviewSendPayload];
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
  leaveChannelCall: [roomJid: string];
  answerDm: [peerJid: string, remoteFullJid: string, sid: string, media: CallMedia];
  reconnectDm: [peerJid: string, media: CallMedia];
  endDm: [peerJid: string, sid?: string];
  refreshUpdate: [];
  loadOlder: [];
  retryLoad: [];
  pinMessage: [messageId: string];
  unpinMessage: [messageId: string];
}>();

// Hide thread members from the main feed — they live inside the thread panel.
const feedMessages = computed(() => props.messages.filter(isFeedTimelineMessage));
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
const callState = useStore($callState);

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

function onSend(
  body: string,
  markup: MarkupSpan[],
  references: MessageReference[],
  files?: Array<File | Blob>,
  linkPreview?: ComposerLinkPreviewSendPayload,
) {
  const pending = replyingTo.value;
  emit(
    "send",
    body,
    markup,
    references,
    files,
    pending ? { id: pending.id, author: pending.author, ...(pending.body ? { body: pending.body } : {}) } : undefined,
    !pending && detectForumChannel(props.channel) ? forumTitle.value : undefined,
    linkPreview,
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

const {
  replyJumpNotice,
  clearReplyJumpNotice,
  showReplyJumpNotice,
  scrollToMessage,
} = useMessageJump({
  messages: () => props.messages,
  ensureMessageLoaded: () => props.ensureMessageLoaded,
  virtualTimeline: () => virtualTimelineRef.value,
  messagesContainer,
});

// "Jump to latest" floating button — shown when the user has scrolled
// away from the live edge. The live-edge anchor flips with the user's
// scroll-direction preference (social = top, chat = bottom), so the
// same button reads "↑ Latest" or "↓ Latest" accordingly.
const jumpToLive = useJumpToLiveEdge({
  scrollElement: computed(() => virtualTimelineRef.value?.scrollElement ?? null),
  mode: scrollDirection,
  scrollToEdge: (mode) => virtualTimelineRef.value?.scrollToPinnedEdge(mode) ?? false,
});
const timelineScrollFrame = createScrollFrameScheduler(() => {
  updateCurrentDayMarker();
  jumpToLive.updateDistance();
});

function onMessagesScroll() {
  timelineScrollFrame.schedule();
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
const linkPreviewScope = computed(() => props.dmPeer?.peerJid ?? props.roomJid ?? null);
const linkPreviewLookup = computed<ComposerLinkPreviewLookup | null>(() => {
  const client = props.xmppClient;
  const scopeJid = linkPreviewScope.value;
  if (!client || !scopeJid) return null;
  return (body: string) => client.lookupLinkPreview(body, scopeJid);
});
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
// The call-anchor card keys its live/ended state on the conversation: the room
// JID for channels, the peer JID for DMs. `callRoomJid` is null for DMs, so a
// DM anchor would otherwise never match its `$dmCallActivities` entry and would
// render "Call ended" while the call is live.
const callAnchorConversationJid = computed(
  () => callRoomJid.value ?? props.dmPeer?.peerJid ?? null,
);
const activeCallThreadId = computed(() =>
  activeCallChatThreadId(callState.value, props.messages, callRoomJid.value),
);

// Messages on the active call's XEP-0201 thread, rendered in the Dock's Chat tab
// (all of them, including the local user's own, so they see what they sent — but
// not the bodyless call-anchor card, which shares the thread id).
const callChatMessages = computed(() =>
  activeCallThreadId.value
    ? props.messages.filter(
        (message) =>
          message.threadId === activeCallThreadId.value && !message.callThread,
      )
    : [],
);

// Drive the Chat tab's unread badge: inbound (non-self) call-thread messages
// accumulate while the Chat tab isn't the focused dock tab and clear once it is.
const callDockOpen = useStore($callDockOpen);
const callDockTab = useStore($callDockTab);
const callChatInboundIds = computed(() =>
  inboundCallChatThreadIds(props.messages, activeCallThreadId.value),
);
// Key the watch on a stable string so it fires only when the call-thread inbound
// ids (or focus) actually change — not on every unrelated conversation message,
// reaction, or edit during a busy call.
watch(
  [
    () => callChatInboundIds.value.join("\n"),
    () => isCallChatTabFocused(callDockOpen.value, callDockTab.value),
  ],
  ([, focused]) => syncCallChatUnread(callChatInboundIds.value, focused),
  { immediate: true },
);

watch(
  () => props.xmppClient,
  (client, previousClient) => {
    if (client !== previousClient) resetExtensionLauncherState({ clearCommands: true });
    if (client) void extensionLauncher.ensureDiscovered();
  },
  { immediate: true },
);
const showSearch = ref(false);
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
  if (props.sidebarMode === "dms") return true;
  const me = props.currentUser;
  if (!me) return false;
  if (props.roomPresence[me] === undefined) return false;
  const channelPolicy = props.channel?.pinPermission ?? "admins-only";
  if (channelPolicy === "anyone") return true;
  const myAffiliation = props.roomAuthority[me]?.affiliation ?? "none";
  return myAffiliation === "owner" || myAffiliation === "admin";
});
const canShowComposer = computed(() =>
  !!(props.channel || props.dmPeer) && !props.channelAccessRequired,
);
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
  // Closing the search panel also resets its internal input state.
  showSearch.value = false;
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
const {
  connectionNotice,
  connectionStatusIcon,
  connectionStatusClasses,
  clearReconnectedNotice,
} = useConnectionNotice({
  status: () => props.xmppStatus,
  queuedMessageCount: () => queuedMessageCount.value,
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
  if (props.xmppClient) void extensionLauncher.ensureDiscovered();
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
  timelineScrollFrame.disconnect();
  clearReconnectedNotice();
  clearReplyJumpNotice();
});

function showDividerBefore(messageId: string): boolean {
  return props.firstUnseenId === messageId && newMessagesDividerPlacement.value === "before";
}

function showDividerAfter(messageId: string): boolean {
  return props.firstUnseenId === messageId && newMessagesDividerPlacement.value === "after";
}

function updateCurrentDayMarker() {
  currentDayMarkerLabel.value = currentDayMarkerLabelFor(messagesContainer.value);
}

watch(
  [orderedFeedMessages, () => props.isLoadingMessages, conversationScope],
  () => {
    void nextTick(updateCurrentDayMarker);
  },
  { flush: "post" },
);

const messageDisplayMeta = computed(() => buildMessageDisplayMeta(orderedFeedMessages.value));

function isGroupedFollowUp(messageId: string): boolean {
  return messageDisplayMeta.value.grouped.has(messageId);
}

function threadChipParticipants(messageId: string) {
  return threadChipParticipantsFor(
    props.threadIndex,
    messageId,
    props.avatarUrlByAuthor,
    props.roomPresence,
  );
}

function threadChipLastReplyAt(messageId: string): string | undefined {
  return threadChipLastReplyAtFor(props.threadIndex, messageId);
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
      :self-full-jid="selfFullJid ?? null"
      @join-channel-call="(channelId, roomJid, media) => emit('joinChannelCall', channelId, roomJid, media)"
      @leave-channel-call="(roomJid) => emit('leaveChannelCall', roomJid)"
      @answer-dm="(peerJid, remoteFullJid, sid, media) => emit('answerDm', peerJid, remoteFullJid, sid, media)"
      @reconnect-dm="(peerJid, media) => emit('reconnectDm', peerJid, media)"
      @end-dm="(peerJid, sid) => emit('endDm', peerJid, sid)"
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

    <MessageSearchPanel
      v-model:open="showSearch"
      :results="searchResults"
      :is-searching="isSearching"
      :avatar-url-by-author="avatarUrlByAuthor"
      :room-presence="roomPresence"
      @search="(query) => emit('search', query)"
      @clear="emit('clearSearch')"
      @open-result="openSearchResult"
    />

    <!-- Error banner -->
    <div
      v-if="actionError && !channelAccessRequired"
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
      :xmpp-client="xmppClient"
      :space-id="waddle?.id"
      :channel-id="channel?.id"
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
      :mention-candidates="mentionCandidates"
      :slow-mode-cooldown="slowModeCooldown"
      :upload-progress="uploadProgress"
      :replying-to="replyingTo"
      :is-top-pinned="true"
      :extensions-open="extensionLauncherOpen"
      :slash-commands="allDiscoveredCommands"
      :in-muc="inMucContext"
      :dispatch-slash-command="dispatchSlashCommand"
      :link-preview-lookup="linkPreviewLookup"
      :link-preview-scope="linkPreviewScope"
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
    <TypingIndicator
      v-if="typingUsers.length > 0 && isTopPinned"
      variant="social"
      :typing-users="typingUsers"
      :avatar-url-by-author="avatarUrlByAuthor"
      :room-presence="roomPresence"
    />

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
      v-if="channelAccessRequired || isLoadingMessages || !channel && !dmPeer || feedMessages.length === 0"
      :ref="setMessagesContainer"
      class="chat-pane-scroll chat-message-scroll flex-1 min-h-0 px-[var(--chat-content-inline)]"
      @scroll="onMessagesScroll"
    >
      <TimelineEmptyState
        v-if="channelAccessRequired"
        variant="access-required"
        :is-forum-channel="isForumChannel"
        :channel-name="channel?.name ?? null"
      />

      <MessageListSkeleton v-else-if="isLoadingMessages" />

      <TimelineEmptyState
        v-else-if="!channel && !dmPeer"
        variant="pick"
        :sidebar-mode="sidebarMode"
        :is-forum-channel="isForumChannel"
      />

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

      <TimelineEmptyState
        v-else-if="feedMessages.length === 0"
        variant="quiet"
        :is-forum-channel="isForumChannel"
        :dm-peer-username="dmPeer?.peerUsername ?? null"
        :channel-name="channel?.name ?? null"
      />

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
            :link-preview-lookup="linkPreviewLookup"
            :link-preview-scope="linkPreviewScope"
            :call-room-jid="callAnchorConversationJid"
            :call-channel-id="channel?.id ?? null"
            @edit="(id, body, m, r, lp) => emit('editMessage', id, body, m, r, lp)"
            @retract="(id) => emit('retractMessage', id)"
            @react="(id, emoji) => emit('reactMessage', id, emoji)"
            @reply="beginReply"
            @scroll-to-message="scrollToMessage"
            @avatar-click="onAvatarClick"
            @open-thread="(tid: string) => emit('openThread', tid)"
            @join-channel-call="(channelId, roomJid, media) => emit('joinChannelCall', channelId, roomJid, media)"
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
    <TypingIndicator
      v-if="typingUsers.length > 0 && !isTopPinned"
      variant="chat"
      :typing-users="typingUsers"
      :avatar-url-by-author="avatarUrlByAuthor"
      :room-presence="roomPresence"
    />

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
      :mention-candidates="mentionCandidates"
      :slow-mode-cooldown="slowModeCooldown"
      :upload-progress="uploadProgress"
      :replying-to="replyingTo"
      :extensions-open="extensionLauncherOpen"
      :slash-commands="allDiscoveredCommands"
      :in-muc="inMucContext"
      :dispatch-slash-command="dispatchSlashCommand"
      :link-preview-lookup="linkPreviewLookup"
      :link-preview-scope="linkPreviewScope"
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
      :call-thread-id="activeCallThreadId"
      :call-chat-messages="callChatMessages"
      :avatar-url-by-author="avatarUrlByAuthor"
      :is-sending="isSending"
      :disabled="!canShowComposer"
      :mention-candidates="mentionCandidates"
      :slow-mode-cooldown="slowModeCooldown"
      :upload-progress="uploadProgress"
      :link-preview-lookup="linkPreviewLookup"
      :link-preview-scope="linkPreviewScope"
      :send-call-chat-message="sendCallChatMessage"
      :xmpp-client="xmppClient"
      :space-id="waddle?.id"
      :channel-id="channel?.id"
      @send-call-chat="(...args) => emit('sendCallChat', ...args)"
    />
  </div>
</template>
