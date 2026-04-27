<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch, type ComponentPublicInstance } from "vue";
import { AlertCircle, CheckCircle2, Hash, LoaderCircle, MessageCircle, MessagesSquare, RefreshCw, Search, Upload, WifiOff, X } from "lucide-vue-next";
import { isForumChannel as detectForumChannel } from "@/lib/channel-types";
import { getConnectionNoticeCopy } from "@/lib/connection-notice";
import { findMessageElementById } from "@/lib/message-targeting";
import { getReplyJumpNotice } from "@/lib/reply-ux";
import {
  getPinnedScrollTop,
  getNewMessagesDividerPlacement,
  orderTimelineForScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";
import { extractFilesFromEvent } from "@/lib/xmpp/file-upload";
import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { ExtensionAnnotationAction, TimelineMessage, MarkupSpan, MessageReference } from "@/lib/chat-ui";
import type { MentionCandidate } from "@/lib/mentions";
import type { BrowserXmppClient, XmppStatusSnapshot, RoomHats, RoomPresence } from "@/lib/xmpp-client";
import {
  extensionCommandOutcome,
  parseExtensionCommandLaunches,
  parseExtensionCommandForm,
  type DiscoveredExtensionCommand,
  type ExtensionCommandFormField,
} from "@/lib/xmpp/extension-commands";
import { useScrollDirection } from "@/composables/useScrollDirection";
import type { ThreadIndex } from "@/composables/useThreads";
import { formatStamp, formatDayDivider, isSameDay } from "@/composables/useMessaging";
import ChatHeader from "@/components/chat/ChatHeader.vue";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import UserProfileDrawer from "@/components/chat/UserProfileDrawer.vue";
import VirtualTimeline from "@/components/chat/VirtualTimeline.vue";

const draft = defineModel<string>("draft", { required: true });
const forumTitle = defineModel<string>("forumTitle", { default: "" });

const props = defineProps<{
  waddle: SpaceSummary | null;
  channel: ChannelSummary | null;
  dmPeer?: { peerJid: string; peerUsername: string; presenceShow?: string } | null;
  sidebarMode?: "channels" | "dms";
  messages: TimelineMessage[];
  firstUnseenId: string | null;
  xmppStatus: XmppStatusSnapshot;
  actionError: string;
  updateAvailable: boolean;
  isApplyingUpdate: boolean;
  isLoadingMessages: boolean;
  isLoadingOlderMessages: boolean;
  hasOlderMessages: boolean;
  isSending: boolean;
  canManageChannels: boolean;
  memberCount: number;
  typingUsers: string[];
  currentUser?: string;
  selfDomain?: string;
  avatarUrlByAuthor: Record<string, string | null>;
  authorJidByNick?: Record<string, string>;
  tenorApiKey: string;
  mentionCandidates: MentionCandidate[];
  roomHats: RoomHats;
  roomPresence: RoomPresence;
  roomLastSeen: Record<string, number>;
  slowModeCooldown: number;
  searchResults: { id: string; nick: string; body: string; createdAt: string }[];
  isSearching: boolean;
  uploadProgress: { uploading: boolean; progress: number; filename: string };
  threadIndex: ThreadIndex;
  xmppClient?: BrowserXmppClient | null;
  reactionMode?: { selectedMessageId: string | null } | null;
  invokeExtensionAction?: (action: ExtensionAnnotationAction) => Promise<unknown>;
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
  openThread: [threadId: string];
  refreshUpdate: [];
  loadOlder: [];
}>();

// Hide thread members from the main feed — they live inside the thread panel.
// Keep thread roots (id === threadId) and any message with no threadId.
const feedMessages = computed(() =>
  props.messages.filter((m) => !m.threadId || m.id === m.threadId),
);
const { mode: scrollDirection, isTopPinned } = useScrollDirection();
const orderedFeedMessages = computed(() =>
  orderTimelineForScrollDirection(feedMessages.value, scrollDirection.value),
);
const newMessagesDividerPlacement = computed(() =>
  getNewMessagesDividerPlacement(scrollDirection.value),
);
const olderSentinelPosition = computed(() => scrollDirection.value === "social" ? "end" : "start");

const replyingTo = ref<{ id: string; author: string; body?: string; preview?: string } | null>(null);
const extensionLauncherOpen = ref(false);
const extensionCommands = ref<DiscoveredExtensionCommand[]>([]);
const extensionLauncherState = ref<"idle" | "loading" | "error">("idle");
const extensionLauncherDetail = ref("");
const extensionCommandStates = ref<Record<string, { state: "loading" | "success" | "warning" | "error"; detail?: string }>>({});
const extensionCommandForms = ref<Record<string, { sessionId: string; fields: ExtensionCommandFormField[] }>>({});
const extensionCommandActions = ref<Record<string, ExtensionAnnotationAction[]>>({});

type MessageComposerHandle = {
  addAttachments: (files: Array<File | Blob>) => void;
  focus: () => void;
};

function focusComposer() {
  // Wait for the reply chip/state update so the editor focus wins over the
  // message action button that was just clicked.
  void nextTick(() => composerRef.value?.focus());
}

async function openExtensionLauncher() {
  extensionLauncherOpen.value = !extensionLauncherOpen.value;
  if (!extensionLauncherOpen.value || extensionCommands.value.length > 0) return;
  if (!props.xmppClient) {
    extensionLauncherState.value = "error";
    extensionLauncherDetail.value = "Extensions are unavailable while XMPP is disconnected.";
    return;
  }
  extensionLauncherState.value = "loading";
  extensionLauncherDetail.value = "";
  try {
    extensionCommands.value = await props.xmppClient.discoverExtensionCommands();
    extensionLauncherState.value = "idle";
    if (extensionCommands.value.length === 0) {
      extensionLauncherDetail.value = "No extension commands discovered.";
    }
  } catch (error) {
    extensionLauncherState.value = "error";
    extensionLauncherDetail.value = error instanceof Error ? error.message : "Could not discover extension commands.";
  }
}

async function invokeExtensionCommand(command: DiscoveredExtensionCommand) {
  if (!props.xmppClient) return;
  const key = command.node;
  extensionCommandStates.value = { ...extensionCommandStates.value, [key]: { state: "loading" } };
  try {
    const result = await props.xmppClient.invokeExtensionCommand(command);
    const outcome = extensionCommandOutcome(result);
    if (result.sessionId && result.form) {
      const fields = parseExtensionCommandForm(result.form);
      if (fields.length > 0) {
        extensionCommandForms.value = {
          ...extensionCommandForms.value,
          [key]: { sessionId: result.sessionId, fields },
        };
      }
    }
    if (result.form) {
      const actions = parseExtensionCommandLaunches(result.form);
      if (actions.length > 0) {
        extensionCommandActions.value = { ...extensionCommandActions.value, [key]: actions };
      }
    }
    extensionCommandStates.value = { ...extensionCommandStates.value, [key]: outcome };
  } catch (error) {
    extensionCommandStates.value = {
      ...extensionCommandStates.value,
      [key]: { state: "error", detail: error instanceof Error ? error.message : "Extension command failed." },
    };
  }
}

async function submitExtensionCommandForm(command: DiscoveredExtensionCommand) {
  if (!props.xmppClient) return;
  const key = command.node;
  const form = extensionCommandForms.value[key];
  if (!form) return;
  extensionCommandStates.value = { ...extensionCommandStates.value, [key]: { state: "loading" } };
  try {
    const result = await props.xmppClient.submitExtensionCommandForm(command, form.sessionId, form.fields);
    const outcome = extensionCommandOutcome(result);
    if (result.form) {
      const actions = parseExtensionCommandLaunches(result.form);
      if (actions.length > 0) {
        extensionCommandActions.value = { ...extensionCommandActions.value, [key]: actions };
      }
    }
    const nextForms = { ...extensionCommandForms.value };
    delete nextForms[key];
    extensionCommandForms.value = nextForms;
    extensionCommandStates.value = { ...extensionCommandStates.value, [key]: outcome };
  } catch (error) {
    extensionCommandStates.value = {
      ...extensionCommandStates.value,
      [key]: { state: "error", detail: error instanceof Error ? error.message : "Extension form submission failed." },
    };
  }
}

async function invokeCommandResultAction(command: DiscoveredExtensionCommand, action: ExtensionAnnotationAction) {
  if (!props.invokeExtensionAction) return;
  const key = command.node;
  extensionCommandStates.value = { ...extensionCommandStates.value, [key]: { state: "loading" } };
  try {
    const result = await props.invokeExtensionAction(action);
    const outcome = extensionCommandOutcome(result);
    extensionCommandStates.value = { ...extensionCommandStates.value, [key]: outcome };
  } catch (error) {
    extensionCommandStates.value = {
      ...extensionCommandStates.value,
      [key]: { state: "error", detail: error instanceof Error ? error.message : "Extension action failed." },
    };
  }
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
  if (await virtualTimelineRef.value?.scrollToMessageId(messageId, "center")) {
    await nextTick();
  }
  const el = findMessageElementById(messagesContainer.value, messageId);
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
  void nextTick(updateCurrentDayMarker);
};
const setVirtualTimelineRef = (
  instance: VirtualTimelineHandle | null,
) => {
  virtualTimelineRef.value = instance;
  setMessagesContainer(instance?.scrollElement ?? null);
};
const currentDayMarkerLabel = ref("");
const composerRef = ref<MessageComposerHandle | null>(null);
const setComposerRef = (instance: MessageComposerHandle | null) => {
  composerRef.value = instance;
};
const showSearch = ref(false);
const searchInput = ref("");
const avatarUrlByAuthor = computed(() => props.avatarUrlByAuthor ?? {});
const isForumChannel = computed(() => detectForumChannel(props.channel));
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
      return {
        banner: "bg-muted/35 text-foreground",
        iconWrap: "border-border/70 bg-background/60 text-muted-foreground/80",
        chip: "border-border/70 bg-transparent text-muted-foreground/80",
        body: "text-muted-foreground",
      };
    case "reconnecting":
      return {
        banner: "bg-warning/10 text-foreground",
        iconWrap: "border-warning/15 bg-background/60 text-warning/80",
        chip: "border-warning/15 bg-transparent text-warning/80",
        body: "text-foreground/75",
      };
    case "error":
      return {
        banner: "bg-destructive/10 text-foreground",
        iconWrap: "border-destructive/15 bg-background/60 text-destructive/80",
        chip: "border-destructive/15 bg-transparent text-destructive/80",
        body: "text-foreground/80",
      };
    case "reconnected":
      return {
        banner: "bg-primary/8 text-foreground",
        iconWrap: "border-primary/12 bg-background/60 text-primary/80",
        chip: "border-primary/12 bg-transparent text-primary/80",
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

defineExpose({ messagesContainer, scrollToPinnedEdge });

function doSearch() {
  emit("search", searchInput.value);
}

function closeSearch() {
  showSearch.value = false;
  searchInput.value = "";
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
  currentDayMarkerLabel.value = createdAt ? formatDayDivider(createdAt) : "";
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
    const sameDay = isSameDay(prev.createdAt, cur.createdAt);
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

function showDayDividerBefore(messageId: string): boolean {
  return messageDisplayMeta.value.dayDivider.has(messageId);
}

function dayDividerLabel(createdAt: string): string {
  return formatDayDivider(createdAt);
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
      :waddle="waddle"
      :channel="channel"
      :dm-peer="dmPeer"
      :is-forum-channel="isForumChannel"
      :can-manage-channels="canManageChannels"
      :member-count="memberCount"
      :connection-notice="connectionNotice"
      :connection-status-classes="connectionStatusClasses"
      :connection-status-icon="connectionStatusIcon"
      @open-nav="emit('openNav')"
      @open-details="emit('openDetails')"
      @edit-channel="emit('editChannel')"
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
          v-model="searchInput"
          placeholder="Search messages…"
          aria-label="Search messages"
          class="type-field flex-1 bg-transparent focus:outline-none placeholder:text-muted-foreground/40"
          @keydown.enter="doSearch"
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
    <div v-if="showSearch && (searchResults.length > 0 || isSearching)" class="border-b border-border glass-surface max-h-56 overflow-auto flex-shrink-0">
      <div class="chat-message-lane">
        <div v-if="isSearching" class="type-caption px-[var(--chat-content-inline)] py-3 text-muted-foreground">
          Searching…
        </div>
        <div v-else class="divide-y divide-border">
          <button
            v-for="result in searchResults"
            :key="result.id"
            class="w-full px-[var(--chat-content-inline)] py-3 text-left hover:bg-muted/50 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/25"
            type="button"
            @click="scrollToMessage(result.id)"
          >
            <div class="flex items-baseline gap-2">
              <span class="type-control">{{ result.nick }}</span>
              <span class="type-meta type-numeric text-muted-foreground">{{ formatStamp(result.createdAt) }}</span>
            </div>
            <p class="type-caption truncate text-muted-foreground">{{ result.body }}</p>
          </button>
        </div>
      </div>
    </div>

    <!-- Error banner -->
    <div
      v-if="actionError"
      class="type-control bg-destructive/10 border-b border-destructive/20 text-destructive animate-fade-in"
    >
      <div class="chat-message-lane px-[var(--chat-content-inline)] py-3">{{ actionError }}</div>
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
      :tenor-api-key="tenorApiKey"
      :mention-candidates="mentionCandidates"
      :slow-mode-cooldown="slowModeCooldown"
      :upload-progress="uploadProgress"
      :replying-to="replyingTo"
      :is-top-pinned="true"
      @send="onSend"
      @cancel-reply="cancelReply"
      @typing="emit('typing')"
      @select-gif="onSelectGif"
      @open-extensions="openExtensionLauncher"
    />

    <div
      v-if="extensionLauncherOpen && isTopPinned"
      class="chat-message-lane border-b border-border bg-background/95 px-[var(--chat-content-inline)] py-3"
    >
      <div class="flex flex-wrap items-center gap-2">
        <span v-if="extensionLauncherState === 'loading'" class="type-caption inline-flex items-center gap-2 text-muted-foreground">
          <LoaderCircle class="h-3.5 w-3.5 animate-spin" aria-hidden="true" />
          Loading extensions
        </span>
        <button
          v-for="command in extensionCommands"
          :key="command.node"
          type="button"
          class="type-caption inline-flex min-h-8 items-center gap-1.5 rounded-md border border-border bg-muted px-2.5 py-1.5 text-foreground hover:bg-muted/70 disabled:opacity-60"
          :disabled="extensionCommandStates[command.node]?.state === 'loading'"
          @click="invokeExtensionCommand(command)"
        >
          <LoaderCircle
            v-if="extensionCommandStates[command.node]?.state === 'loading'"
            class="h-3.5 w-3.5 animate-spin"
            aria-hidden="true"
          />
          <CheckCircle2
            v-else-if="extensionCommandStates[command.node]?.state === 'success'"
            class="h-3.5 w-3.5 text-success"
            aria-hidden="true"
          />
          <AlertCircle
            v-else-if="extensionCommandStates[command.node]?.state === 'warning' || extensionCommandStates[command.node]?.state === 'error'"
            class="h-3.5 w-3.5"
            :class="extensionCommandStates[command.node]?.state === 'error' ? 'text-destructive' : 'text-warning'"
            aria-hidden="true"
          />
          {{ command.name }}
        </button>
      </div>
      <p
        v-if="extensionLauncherDetail || Object.values(extensionCommandStates).some((state) => state.detail)"
        class="type-caption mt-2 text-muted-foreground"
      >
        {{ extensionLauncherDetail || Object.values(extensionCommandStates).find((state) => state.detail)?.detail }}
      </p>
      <div
        v-for="command in extensionCommands.filter((item) => extensionCommandForms[item.node])"
        :key="`form:${command.node}`"
        class="mt-3 grid gap-2 rounded-md border border-border bg-muted/30 p-3"
      >
        <label
          v-for="field in extensionCommandForms[command.node].fields"
          :key="`${command.node}:${field.name}`"
          class="type-caption grid gap-1 text-muted-foreground"
        >
          <span>{{ field.label }}</span>
          <input
            v-model="field.value"
            class="min-h-8 rounded-md border border-border bg-background px-2 text-foreground"
            :type="field.type === 'text-private' ? 'password' : 'text'"
            :required="field.required"
            @input="field.values = [field.value]"
          />
        </label>
        <button
          type="button"
          class="type-caption justify-self-start rounded-md bg-primary px-3 py-1.5 text-primary-foreground disabled:opacity-60"
          :disabled="extensionCommandStates[command.node]?.state === 'loading'"
          @click="submitExtensionCommandForm(command)"
        >
          Submit
        </button>
      </div>
      <div
        v-for="command in extensionCommands.filter((item) => extensionCommandActions[item.node]?.length)"
        :key="`actions:${command.node}`"
        class="mt-3 flex flex-wrap gap-2"
      >
        <button
          v-for="action in extensionCommandActions[command.node]"
          :key="`${command.node}:${action.route}`"
          type="button"
          class="type-caption inline-flex min-h-8 items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1.5 text-foreground hover:bg-muted disabled:opacity-60"
          :disabled="extensionCommandStates[command.node]?.state === 'loading'"
          @click="invokeCommandResultAction(command, action)"
        >
          {{ action.label }}
        </button>
      </div>
    </div>

    <!-- Typing indicator (social / top-pinned mode) -->
    <div
      v-if="typingUsers.length > 0 && isTopPinned"
      class="type-caption px-[var(--chat-content-inline)] py-2 text-muted-foreground flex-shrink-0 border-b border-border"
    >
      <div class="chat-message-lane flex items-center gap-2">
        <span class="flex gap-0.5">
          <span class="typing-dot" />
          <span class="typing-dot" />
          <span class="typing-dot" />
        </span>
        <span v-if="typingUsers.length === 1">{{ typingUsers[0] }} is typing</span>
        <span v-else-if="typingUsers.length === 2">{{ typingUsers[0] }} and {{ typingUsers[1] }} are typing</span>
        <span v-else>{{ typingUsers[0] }} and {{ typingUsers.length - 1 }} others are typing</span>
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
      class="chat-pane-scroll chat-message-scroll flex-1 min-h-0 overflow-auto px-[var(--chat-content-inline)]"
      @scroll="updateCurrentDayMarker"
    >
      <div v-if="isLoadingMessages" class="type-caption flex flex-col items-center justify-center gap-3 py-16 text-center text-muted-foreground">
        <div class="flex items-center justify-center gap-1.5">
          <span class="typing-dot" />
          <span class="typing-dot" />
          <span class="typing-dot" />
        </div>
        <p class="text-muted-foreground/60">Loading messages…</p>
      </div>

      <div v-else-if="!channel && !dmPeer" class="chat-empty-state">
        <div class="w-12 h-12 rounded-lg bg-muted flex items-center justify-center">
          <component :is="sidebarMode === 'dms' ? MessageCircle : isForumChannel ? MessagesSquare : Hash" class="w-5 h-5 text-primary/50" />
        </div>
        <p class="type-empty-title text-muted-foreground">
          {{ sidebarMode === "dms"
            ? "Select a conversation"
            : isForumChannel
              ? "Select a forum to browse topics"
              : "Select a channel to start chatting" }}
        </p>
      </div>

      <div v-else-if="feedMessages.length === 0" class="chat-empty-state">
        <div class="w-12 h-12 rounded-lg bg-primary/10 flex items-center justify-center">
          <component :is="dmPeer ? MessageCircle : isForumChannel ? MessagesSquare : Hash" class="w-5 h-5 text-primary" />
        </div>
        <div class="chat-field-stack">
          <p class="type-empty-title">
            {{ dmPeer ? `Conversation with @${dmPeer.peerUsername}` : `Welcome to #${channel?.name}` }}
          </p>
          <p class="type-field text-muted-foreground">
            {{ isForumChannel
              ? "Start the first topic with a clear title so people can follow the thread."
              : "This is the start of the conversation." }}
          </p>
        </div>
      </div>

    </div>

    <VirtualTimeline
      v-else
      :ref="setVirtualTimelineRef"
      :items="orderedFeedMessages"
      :has-older="hasOlderMessages"
      :loading-older="isLoadingOlderMessages"
      :sentinel-position="olderSentinelPosition"
      aria-label="Messages"
      @scroll="updateCurrentDayMarker"
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
            class="type-section-label flex items-center gap-3 py-2 text-destructive"
            data-new-messages-divider
            role="separator"
            aria-label="New messages"
          >
            <div class="flex-1 h-px bg-destructive/40" />
            <span>New messages</span>
            <div class="flex-1 h-px bg-destructive/40" />
          </div>
          <MessageCard
            :message="msg"
            :current-user="props.currentUser"
            :avatar-url="avatarUrlByAuthor[msg.author] ?? null"
            :hats="roomHats[msg.author] ?? []"
            :presence="roomPresence[msg.author] ?? 'offline'"
            :last-seen="roomLastSeen[msg.author]"
            :author-jid="authorJidByNick?.[msg.author]"
            :thread-reply-count="threadIndex.get(msg.id)?.count ?? 0"
            :grouped="isGroupedFollowUp(msg.id)"
            :reaction-mode-selected="reactionMode?.selectedMessageId === msg.id"
            :invoke-extension-action="props.invokeExtensionAction"
            @edit="(id, body, m, r) => emit('editMessage', id, body, m, r)"
            @retract="(id) => emit('retractMessage', id)"
            @react="(id, emoji) => emit('reactMessage', id, emoji)"
            @reply="beginReply"
            @scroll-to-message="scrollToMessage"
            @avatar-click="onAvatarClick"
            @open-thread="(tid: string) => emit('openThread', tid)"
          />
          <div
            v-if="showDividerAfter(msg.id)"
            class="type-section-label flex items-center gap-3 py-2 text-destructive"
            data-new-messages-divider
            role="separator"
            aria-label="New messages"
          >
            <div class="flex-1 h-px bg-destructive/40" />
            <span>New messages</span>
            <div class="flex-1 h-px bg-destructive/40" />
          </div>
      </template>
    </VirtualTimeline>

    <!-- Typing indicator -->
    <div
      v-if="typingUsers.length > 0 && !isTopPinned"
      class="type-caption px-[var(--chat-content-inline)] py-2 text-muted-foreground flex-shrink-0"
    >
      <div class="chat-message-lane flex items-center gap-2">
        <span class="flex gap-0.5">
          <span class="typing-dot" />
          <span class="typing-dot" />
          <span class="typing-dot" />
        </span>
        <span v-if="typingUsers.length === 1">{{ typingUsers[0] }} is typing</span>
        <span v-else-if="typingUsers.length === 2">{{ typingUsers[0] }} and {{ typingUsers[1] }} are typing</span>
        <span v-else>{{ typingUsers[0] }} and {{ typingUsers.length - 1 }} others are typing</span>
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
      :tenor-api-key="tenorApiKey"
      :mention-candidates="mentionCandidates"
      :slow-mode-cooldown="slowModeCooldown"
      :upload-progress="uploadProgress"
      :replying-to="replyingTo"
      @send="onSend"
      @cancel-reply="cancelReply"
      @typing="emit('typing')"
      @select-gif="onSelectGif"
      @open-extensions="openExtensionLauncher"
    />
    <div
      v-if="extensionLauncherOpen && !isTopPinned"
      class="border-t border-border bg-background/95 px-[var(--chat-content-inline)] py-3"
    >
      <div class="flex flex-wrap items-center gap-2">
        <span v-if="extensionLauncherState === 'loading'" class="type-caption inline-flex items-center gap-2 text-muted-foreground">
          <LoaderCircle class="h-3.5 w-3.5 animate-spin" aria-hidden="true" />
          Loading extensions
        </span>
        <button
          v-for="command in extensionCommands"
          :key="command.node"
          type="button"
          class="type-caption inline-flex min-h-8 items-center gap-1.5 rounded-md border border-border bg-muted px-2.5 py-1.5 text-foreground hover:bg-muted/70 disabled:opacity-60"
          :disabled="extensionCommandStates[command.node]?.state === 'loading'"
          @click="invokeExtensionCommand(command)"
        >
          <LoaderCircle
            v-if="extensionCommandStates[command.node]?.state === 'loading'"
            class="h-3.5 w-3.5 animate-spin"
            aria-hidden="true"
          />
          <CheckCircle2
            v-else-if="extensionCommandStates[command.node]?.state === 'success'"
            class="h-3.5 w-3.5 text-success"
            aria-hidden="true"
          />
          <AlertCircle
            v-else-if="extensionCommandStates[command.node]?.state === 'warning' || extensionCommandStates[command.node]?.state === 'error'"
            class="h-3.5 w-3.5"
            :class="extensionCommandStates[command.node]?.state === 'error' ? 'text-destructive' : 'text-warning'"
            aria-hidden="true"
          />
          {{ command.name }}
        </button>
      </div>
      <p
        v-if="extensionLauncherDetail || Object.values(extensionCommandStates).some((state) => state.detail)"
        class="type-caption mt-2 text-muted-foreground"
      >
        {{ extensionLauncherDetail || Object.values(extensionCommandStates).find((state) => state.detail)?.detail }}
      </p>
      <div
        v-for="command in extensionCommands.filter((item) => extensionCommandForms[item.node])"
        :key="`form:${command.node}`"
        class="mt-3 grid gap-2 rounded-md border border-border bg-muted/30 p-3"
      >
        <label
          v-for="field in extensionCommandForms[command.node].fields"
          :key="`${command.node}:${field.name}`"
          class="type-caption grid gap-1 text-muted-foreground"
        >
          <span>{{ field.label }}</span>
          <input
            v-model="field.value"
            class="min-h-8 rounded-md border border-border bg-background px-2 text-foreground"
            :type="field.type === 'text-private' ? 'password' : 'text'"
            :required="field.required"
            @input="field.values = [field.value]"
          />
        </label>
        <button
          type="button"
          class="type-caption justify-self-start rounded-md bg-primary px-3 py-1.5 text-primary-foreground disabled:opacity-60"
          :disabled="extensionCommandStates[command.node]?.state === 'loading'"
          @click="submitExtensionCommandForm(command)"
        >
          Submit
        </button>
      </div>
      <div
        v-for="command in extensionCommands.filter((item) => extensionCommandActions[item.node]?.length)"
        :key="`actions:${command.node}`"
        class="mt-3 flex flex-wrap gap-2"
      >
        <button
          v-for="action in extensionCommandActions[command.node]"
          :key="`${command.node}:${action.route}`"
          type="button"
          class="type-caption inline-flex min-h-8 items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1.5 text-foreground hover:bg-muted disabled:opacity-60"
          :disabled="extensionCommandStates[command.node]?.state === 'loading'"
          @click="invokeCommandResultAction(command, action)"
        >
          {{ action.label }}
        </button>
      </div>
    </div>
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
  </div>
</template>
