<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import { AlertCircle, CheckCircle2, Hash, MessageCircle, MessagesSquare, RefreshCw, Settings, Search, Upload, WifiOff, X } from "lucide-vue-next";
import { isForumChannel as detectForumChannel } from "@/lib/channel-types";
import { getConnectionNoticeCopy } from "@/lib/connection-notice";
import { findMessageElementById } from "@/lib/message-targeting";
import { getReplyJumpNotice } from "@/lib/reply-ux";
import {
  getNewMessagesDividerPlacement,
  orderTimelineForScrollDirection,
} from "@/lib/scroll-direction";
import { extractImagesFromEvent } from "@/lib/xmpp/file-upload";
import type { ChannelSummary, WaddleSummary } from "@/lib/waddle-api";
import type { TimelineMessage, MarkupSpan } from "@/lib/chat-ui";
import type { XmppStatusSnapshot, RoomHats, RoomPresence } from "@/lib/xmpp-client";
import { useScrollDirection } from "@/composables/useScrollDirection";
import type { ThreadIndex } from "@/composables/useThreads";
import { formatStamp } from "@/composables/useMessaging";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import UserPopover from "@/components/chat/UserPopover.vue";

const draft = defineModel<string>("draft", { required: true });
const forumTitle = defineModel<string>("forumTitle", { default: "" });

const props = defineProps<{
  waddle: WaddleSummary | null;
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
  isSending: boolean;
  canManageChannels: boolean;
  typingUsers: string[];
  currentUser?: string;
  selfDomain?: string;
  avatarUrlByAuthor: Record<string, string | null>;
  authorJidByNick?: Record<string, string>;
  tenorApiKey: string;
  memberNames: string[];
  roomHats: RoomHats;
  roomPresence: RoomPresence;
  roomLastSeen: Record<string, number>;
  slowModeCooldown: number;
  searchResults: { id: string; nick: string; body: string; createdAt: string }[];
  isSearching: boolean;
  uploadProgress: { uploading: boolean; progress: number; filename: string };
  threadIndex: ThreadIndex;
}>();

const emit = defineEmits<{
  send: [
    body: string,
    markup: MarkupSpan[],
    files?: Array<File | Blob>,
    replyTo?: { id: string; author: string; body?: string },
    forumTitle?: string,
  ];
  typing: [];
  editMessage: [messageId: string, newBody: string, markup?: MarkupSpan[]];
  retractMessage: [messageId: string];
  reactMessage: [messageId: string, emoji: string];
  displayed: [messageId: string];
  editChannel: [];
  search: [query: string];
  clearSearch: [];
  openDm: [peerJid: string];
  openThread: [threadId: string];
  refreshUpdate: [];
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

const replyingTo = ref<{ id: string; author: string; body?: string; preview?: string } | null>(null);

type MessageComposerHandle = {
  addAttachments: (files: Array<File | Blob>) => void;
  focus: () => void;
};

function focusComposer() {
  // Wait for the reply chip/state update so the editor focus wins over the
  // message action button that was just clicked.
  void nextTick(() => composerRef.value?.focus());
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

function scrollToMessage(messageId: string) {
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

function onSend(body: string, markup: MarkupSpan[], files?: Array<File | Blob>) {
  const pending = replyingTo.value;
  emit(
    "send",
    body,
    markup,
    files,
    pending ? { id: pending.id, author: pending.author, ...(pending.body ? { body: pending.body } : {}) } : undefined,
    !pending && detectForumChannel(props.channel) ? forumTitle.value : undefined,
  );
  // Don't clear replyingTo here — wait for send to complete (success or failure)
}

function onSelectGif(url: string) {
  onSend(url, []);
}

const messagesContainer = ref<HTMLDivElement | null>(null);
const setMessagesContainer = (el: HTMLDivElement | null) => {
  messagesContainer.value = el;
};
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
        iconWrap: "border-border bg-background/70 text-muted-foreground",
        chip: "border-border bg-background/75 text-muted-foreground",
        body: "text-muted-foreground",
      };
    case "reconnecting":
      return {
        banner: "bg-warning/10 text-foreground",
        iconWrap: "border-warning/20 bg-background/75 text-warning",
        chip: "border-warning/20 bg-warning/10 text-warning",
        body: "text-foreground/75",
      };
    case "error":
      return {
        banner: "bg-destructive/10 text-foreground",
        iconWrap: "border-destructive/20 bg-background/75 text-destructive",
        chip: "border-destructive/20 bg-destructive/10 text-destructive",
        body: "text-foreground/80",
      };
    case "reconnected":
      return {
        banner: "bg-primary/8 text-foreground",
        iconWrap: "border-primary/15 bg-background/75 text-primary",
        chip: "border-primary/15 bg-primary/8 text-primary",
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

defineExpose({ messagesContainer });

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

function onAvatarClick(author: string) {
  if (author === props.currentUser) return;
  const authorJid = props.authorJidByNick?.[author]
    ?? (props.selfDomain ? `${author}@${props.selfDomain}` : null);
  if (!authorJid) return;
  popoverAuthor.value = { username: author, jid: authorJid };
}

function closePopover() {
  popoverAuthor.value = null;
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
  const files = extractImagesFromEvent(e);
  if (files.length > 0) composerRef.value?.addAttachments(files);
}

// XEP-0333: Send displayed marker for the latest non-self message
watch(
  () => props.messages,
  (msgs) => {
    const last = [...msgs].reverse().find((m) => !m.isSelf && !m.isRetracted);
    if (last) {
      emit("displayed", last.id);
    }
  },
  { deep: true },
);

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
      class="absolute inset-0 z-50 bg-primary/10 border-2 border-dashed border-primary rounded-xl flex flex-col items-center justify-center pointer-events-none animate-fade-in"
    >
      <Upload class="w-8 h-8 text-primary mb-2" />
      <span class="text-primary font-display font-bold text-lg">Drop image to upload</span>
    </div>

    <!-- Header — sleek, floating feel -->
    <div class="h-14 border-b border-border px-6 flex items-center justify-between gap-3 flex-shrink-0 glass-surface">
      <div class="flex items-center gap-3">
        <div class="flex items-center gap-2">
          <component :is="dmPeer ? MessageCircle : isForumChannel ? MessagesSquare : Hash" class="w-4 h-4 text-primary/70" />
          <h1 class="text-[15px] font-display font-bold tracking-tight">
            {{ dmPeer ? dmPeer.peerUsername : channel?.name ?? "..." }}
          </h1>
          <span
            v-if="isForumChannel"
            class="rounded-full border border-primary/15 bg-primary/8 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-[0.14em] text-primary/80"
          >
            Forum
          </span>
          <span v-if="dmPeer" class="text-[11px] text-muted-foreground">· {{ presenceText(dmPeer.presenceShow) }}</span>
        </div>
      </div>
      <div class="flex items-center gap-2">
        <div
          v-if="connectionNotice && connectionStatusClasses"
          class="hidden md:inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[11px] font-medium tracking-tight"
          :class="connectionStatusClasses.chip"
        >
          <component
            :is="connectionStatusIcon"
            class="w-3.5 h-3.5"
            :class="{ 'motion-safe:animate-spin': connectionNotice.tone === 'reconnecting' }"
          />
          <span>{{ connectionNotice.shortLabel }}</span>
        </div>
        <div class="flex gap-1">
          <button
            v-if="channel || dmPeer"
            class="h-8 w-8 flex items-center justify-center rounded-lg transition-all duration-200"
            :class="showSearch ? 'bg-muted text-primary' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
            title="Search messages"
            @click="showSearch = !showSearch"
          >
            <Search class="w-3.5 h-3.5" />
          </button>
          <button
            v-if="canManageChannels && channel"
            class="h-8 w-8 flex items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground transition-all duration-200"
            title="Channel settings"
            @click="emit('editChannel')"
          >
            <Settings class="w-3.5 h-3.5" />
          </button>
        </div>
      </div>
    </div>
    <div
      v-if="connectionNotice && connectionStatusClasses"
      role="status"
      aria-live="polite"
      aria-atomic="true"
      class="border-b border-border/80 animate-fade-in"
      :class="connectionStatusClasses.banner"
    >
      <div class="px-6 py-3 flex items-start gap-3">
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
        <div class="min-w-0 space-y-0.5">
          <p class="text-[13px] font-medium tracking-tight">
            {{ connectionNotice.title }}
          </p>
          <p class="max-w-[72ch] text-[12px] leading-5" :class="connectionStatusClasses.body">
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
      <div class="px-6 py-3 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div class="flex min-w-0 items-start gap-3">
          <div class="mt-0.5 flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full border border-primary/15 bg-background/75 text-primary">
            <RefreshCw
              class="h-4 w-4"
              :class="{ 'motion-safe:animate-spin': isApplyingUpdate }"
            />
          </div>
          <div class="min-w-0 space-y-0.5">
            <p class="text-[13px] font-medium tracking-tight">
              Update ready
            </p>
            <p class="max-w-[72ch] text-[12px] leading-5 text-foreground/75">
              {{ updateNoticeBody }}
            </p>
          </div>
        </div>
        <button
          type="button"
          class="inline-flex h-8 shrink-0 items-center justify-center gap-1.5 rounded-full bg-primary px-3.5 text-[12px] font-medium text-primary-foreground transition-colors hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/35 disabled:cursor-wait disabled:opacity-75"
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
    <div v-if="showSearch" class="px-6 py-2.5 border-b border-border glass-surface flex items-center gap-2.5 flex-shrink-0 animate-fade-in">
      <Search class="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
      <input
        v-model="searchInput"
        placeholder="Search messages..."
        class="flex-1 text-[13px] bg-transparent focus:outline-none placeholder:text-muted-foreground/40"
        @keydown.enter="doSearch"
      />
      <button
        v-if="searchInput"
        class="p-0.5 rounded text-muted-foreground hover:text-foreground transition-colors"
        @click="closeSearch"
      >
        <X class="w-3.5 h-3.5" />
      </button>
    </div>

    <!-- Search results -->
    <div v-if="showSearch && (searchResults.length > 0 || isSearching)" class="border-b border-border glass-surface max-h-56 overflow-auto flex-shrink-0">
      <div v-if="isSearching" class="px-6 py-3 text-[13px] text-muted-foreground">
        Searching...
      </div>
      <div v-else class="divide-y divide-border">
        <div
          v-for="result in searchResults"
          :key="result.id"
          class="px-6 py-3 hover:bg-muted/50 transition-colors cursor-pointer"
        >
          <div class="flex items-baseline gap-2">
            <span class="font-medium text-[12px]">{{ result.nick }}</span>
            <span class="text-[11px] font-mono text-muted-foreground tabular-nums">{{ formatStamp(result.createdAt) }}</span>
          </div>
          <p class="text-[12px] text-muted-foreground truncate mt-0.5">{{ result.body }}</p>
        </div>
      </div>
    </div>

    <!-- Error banner -->
    <div
      v-if="actionError"
      class="px-6 py-2.5 bg-destructive/10 border-b border-destructive/20 text-[13px] text-destructive animate-fade-in"
    >
      <div>{{ actionError }}</div>
    </div>
    <div
      v-if="replyJumpNotice"
      role="status"
      aria-live="polite"
      aria-atomic="true"
      class="px-6 py-2 text-[12px] text-muted-foreground bg-muted/35 border-b border-border animate-fade-in"
    >
      {{ replyJumpNotice }}
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
      :member-names="memberNames"
      :slow-mode-cooldown="slowModeCooldown"
      :upload-progress="uploadProgress"
      :replying-to="replyingTo"
      :is-top-pinned="true"
      @send="onSend"
      @cancel-reply="cancelReply"
      @typing="emit('typing')"
      @select-gif="onSelectGif"
    />

    <!-- Typing indicator (social / top-pinned mode) -->
    <div
      v-if="typingUsers.length > 0 && isTopPinned"
      class="px-6 py-1.5 text-[11px] text-muted-foreground flex items-center gap-2 flex-shrink-0 border-b border-border"
    >
      <span class="flex gap-0.5">
        <span class="typing-dot" />
        <span class="typing-dot" />
        <span class="typing-dot" />
      </span>
      <span v-if="typingUsers.length === 1">{{ typingUsers[0] }} is typing</span>
      <span v-else-if="typingUsers.length === 2">{{ typingUsers[0] }} and {{ typingUsers[1] }} are typing</span>
      <span v-else>{{ typingUsers[0] }} and {{ typingUsers.length - 1 }} others are typing</span>
    </div>

    <!-- Messages -->
    <div :ref="setMessagesContainer" class="flex-1 min-h-0 overflow-auto px-6 py-4">
      <div v-if="isLoadingMessages" class="text-center py-16 text-[13px] text-muted-foreground">
        <div class="flex items-center justify-center gap-1.5">
          <span class="typing-dot" />
          <span class="typing-dot" />
          <span class="typing-dot" />
        </div>
        <p class="mt-3 text-muted-foreground/60">Loading messages...</p>
      </div>

      <div v-else-if="!channel && !dmPeer" class="flex flex-col items-center justify-center py-20">
        <div class="w-12 h-12 rounded-xl bg-muted flex items-center justify-center mb-4">
          <component :is="sidebarMode === 'dms' ? MessageCircle : isForumChannel ? MessagesSquare : Hash" class="w-5 h-5 text-primary/50" />
        </div>
        <p class="text-[14px] text-muted-foreground font-display">
          {{ sidebarMode === "dms"
            ? "Select a conversation"
            : isForumChannel
              ? "Select a forum to browse topics"
              : "Select a channel to start chatting" }}
        </p>
      </div>

      <div v-else-if="feedMessages.length === 0" class="flex flex-col items-center justify-center py-20">
        <div class="w-12 h-12 rounded-xl bg-primary/10 flex items-center justify-center mb-4">
          <component :is="dmPeer ? MessageCircle : isForumChannel ? MessagesSquare : Hash" class="w-5 h-5 text-primary" />
        </div>
        <p class="text-[16px] font-display font-bold mb-1">
          {{ dmPeer ? `Conversation with @${dmPeer.peerUsername}` : `Welcome to #${channel?.name}` }}
        </p>
        <p class="text-[13px] text-muted-foreground">
          {{ isForumChannel
            ? "Start the first topic with a clear title so people can follow the thread."
            : "This is the start of the conversation." }}
        </p>
      </div>

      <div v-else class="max-w-none">
        <template v-for="msg in orderedFeedMessages" :key="msg.id">
          <div
            v-if="showDividerBefore(msg.id)"
            class="flex items-center gap-3 py-2 text-[11px] font-medium uppercase tracking-wide text-destructive"
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
            @edit="(id, body, m) => emit('editMessage', id, body, m)"
            @retract="(id) => emit('retractMessage', id)"
            @react="(id, emoji) => emit('reactMessage', id, emoji)"
            @reply="beginReply"
            @scroll-to-message="scrollToMessage"
            @avatar-click="onAvatarClick"
            @open-thread="(tid: string) => emit('openThread', tid)"
          />
          <div
            v-if="showDividerAfter(msg.id)"
            class="flex items-center gap-3 py-2 text-[11px] font-medium uppercase tracking-wide text-destructive"
            data-new-messages-divider
            role="separator"
            aria-label="New messages"
          >
            <div class="flex-1 h-px bg-destructive/40" />
            <span>New messages</span>
            <div class="flex-1 h-px bg-destructive/40" />
          </div>
        </template>
      </div>
    </div>

    <!-- Typing indicator -->
    <div
      v-if="typingUsers.length > 0 && !isTopPinned"
      class="px-6 py-1.5 text-[11px] text-muted-foreground flex items-center gap-2 flex-shrink-0"
    >
      <span class="flex gap-0.5">
        <span class="typing-dot" />
        <span class="typing-dot" />
        <span class="typing-dot" />
      </span>
      <span v-if="typingUsers.length === 1">{{ typingUsers[0] }} is typing</span>
      <span v-else-if="typingUsers.length === 2">{{ typingUsers[0] }} and {{ typingUsers[1] }} are typing</span>
      <span v-else>{{ typingUsers[0] }} and {{ typingUsers.length - 1 }} others are typing</span>
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
      :member-names="memberNames"
      :slow-mode-cooldown="slowModeCooldown"
      :upload-progress="uploadProgress"
      :replying-to="replyingTo"
      @send="onSend"
      @cancel-reply="cancelReply"
      @typing="emit('typing')"
      @select-gif="onSelectGif"
    />
    <UserPopover
      :open="!!popoverAuthor"
      :username="popoverAuthor?.username ?? ''"
      :avatar-url="popoverAuthor ? avatarUrlByAuthor[popoverAuthor.username] ?? null : null"
      :presence-text="dmPeer?.peerUsername === popoverAuthor?.username ? presenceText(dmPeer?.presenceShow) : undefined"
      :can-message="!!popoverAuthor"
      @close="closePopover"
      @message="openPopoverDm"
    />
  </div>
</template>
