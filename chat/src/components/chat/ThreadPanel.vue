<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch, type ComponentPublicInstance, type Ref } from "vue";
import { X, CornerDownRight, ChevronRight, ChevronLeft } from "lucide-vue-next";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import VirtualTimeline from "@/components/chat/VirtualTimeline.vue";
import type { ExtensionAnnotationAction, TimelineMessage, MarkupSpan, MessageReference } from "@/lib/chat-ui";
import type { MentionCandidate } from "@/lib/mentions";
import type { OccupantHat, OccupantPresence, RoomHats, RoomPresence } from "@/lib/xmpp-client";
import type { MessageThreadEntry, MessageThreadIndex } from "@/channels/threads";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import {
  isTopPinnedScrollDirection,
  orderTimelineForScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";
import { createPinnedEdgeScroller } from "@/lib/pinned-edge-scroll";
import { formatTimelineDayDivider, isSameTimelineDay } from "@/channels/timeline";

const props = defineProps<{
  threadStack: string[];
  threadIndex: MessageThreadIndex;
  /**
   * Resolves a thread entry, synthesising an empty one when the id points
   * at a known message with no replies yet (e.g. a freshly-started
   * sub-thread). Returns undefined when the root hasn't been loaded.
   */
  resolveEntry: (threadId: string) => MessageThreadEntry | undefined;
  currentUser?: string;
  currentUserJid?: string;
  avatarUrlByAuthor: Record<string, string | null>;
  authorJidByNick?: Record<string, string>;
  roomHats: RoomHats;
  roomPresence: RoomPresence;
  roomLastSeen: Record<string, number>;
  giphyApiKey: string;
  mentionCandidates: MentionCandidate[];
  slowModeCooldown: number;
  isSending: boolean;
  isLoadingOlderReplies?: boolean;
  hasOlderReplies?: boolean;
  uploadProgress: { uploading: boolean; progress: number; filename: string };
  channelName: string;
  reactionMode?: { selectedMessageId: string | null } | null;
  targetMessageId?: string | null;
  /**
   * When true, the composer is hidden but sub-thread navigation stays active.
   * Used to render the parent context pane in the accordion layout.
   */
  hideComposer?: boolean;
  invokeExtensionAction?: (action: ExtensionAnnotationAction) => Promise<unknown>;
}>();

const emit = defineEmits<{
  close: [];
  popTo: [index: number];
  pushThread: [threadId: string];
  send: [
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files: Array<File | Blob> | undefined,
    replyTo: { id: string; author: string; body?: string } | undefined,
    threadOverride: { threadId: string; parentThreadId?: string },
  ];
  editMessage: [messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[]];
  retractMessage: [messageId: string];
  reactMessage: [messageId: string, emoji: string];
  displayed: [messageId: string];
  selectGif: [url: string];
  typing: [];
  loadOlder: [threadId: string];
}>();

const { mode: scrollDirectionMode } = useScrollDirectionPreference();

const activeThreadId = computed(() => props.threadStack[props.threadStack.length - 1] ?? null);
const parentThreadId = computed(() =>
  props.threadStack.length >= 2 ? props.threadStack[props.threadStack.length - 2] : undefined,
);
const activeEntry = computed(() =>
  activeThreadId.value ? props.resolveEntry(activeThreadId.value) ?? null : null,
);

const orderedChildren = computed(() => {
  const children = activeEntry.value?.directChildren ?? [];
  return orderTimelineForScrollDirection(children, scrollDirectionMode.value);
});
const orderedThreadMessages = computed(() => {
  const root = activeEntry.value?.root;
  if (!root) return [...orderedChildren.value];
  // The root is always the oldest message in the thread. Place it at whichever
  // end of the rendered list represents "oldest" for the active scroll mode:
  // bottom in social/top-pinned mode, top in chat/bottom-pinned mode.
  return isTopPinnedScrollDirection(scrollDirectionMode.value)
    ? [...orderedChildren.value, root]
    : [root, ...orderedChildren.value];
});
const newestThreadMessageId = computed(() =>
  activeEntry.value?.directChildren.at(-1)?.id
    ?? activeEntry.value?.root?.id
    ?? null,
);

// Burst window matches the main feed (ContentArea.vue): same author + < 5 min
// apart + same day, no intervening other-author message in rendered order.
const BURST_WINDOW_MS = 5 * 60 * 1000;

const threadDisplayMeta = computed(() => {
  const grouped = new Set<string>();
  const dayDivider = new Set<string>();
  const sequence = orderedThreadMessages.value;
  for (let i = 0; i < sequence.length; i++) {
    const cur = sequence[i];
    if (!cur) continue;
    const prev = i > 0 ? sequence[i - 1] : null;
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
  return threadDisplayMeta.value.grouped.has(messageId);
}

function showDayDividerBefore(messageId: string): boolean {
  return threadDisplayMeta.value.dayDivider.has(messageId);
}

function dayDividerLabel(createdAt: string): string {
  return formatTimelineDayDivider(createdAt);
}

const isTopPinned = computed(() =>
  isTopPinnedScrollDirection(scrollDirectionMode.value),
);
// Older replies paginate at the chronologically-older end of the list. In
// social mode the root is appended after the loaded replies, so the sentinel
// has to sit one slot inboard of the trailing root rather than below it.
const olderSentinelPosition = computed<"start" | "end" | "before-end">(() => {
  if (scrollDirectionMode.value !== "social") return "start";
  return activeEntry.value?.root ? "before-end" : "end";
});

const scrollContainerRef = ref<HTMLElement | null>(null);
const virtualTimelineEdgeScroller: Ref<((mode: ScrollDirectionMode) => boolean | Promise<boolean>) | null> = ref(null);
const pinnedEdgeScroller = createPinnedEdgeScroller({
  element: scrollContainerRef,
  mode: scrollDirectionMode,
  virtualScroll: virtualTimelineEdgeScroller,
});
const currentDayMarkerLabel = ref("");
const draft = ref("");
const replyingTo = ref<{ id: string; author: string; body?: string } | null>(null);
let olderLoadSnapshot: {
  element: HTMLElement;
  height: number;
  mode: ScrollDirectionMode;
  threadId: string | null;
  top: number;
  token: number;
} | null = null;
let olderLoadRestoreToken = 0;

type ComposerHandle = { focus: () => void };
const composerRef = ref<ComposerHandle | null>(null);
type VirtualTimelineHandle = ComponentPublicInstance & {
  scrollElement: HTMLDivElement | null;
  scrollToMessageId: (messageId: string, align?: "start" | "center" | "end") => Promise<boolean>;
};
const virtualTimelineRef = ref<VirtualTimelineHandle | null>(null);

function setComposerRef(el: ComposerHandle | null) {
  composerRef.value = el;
}

async function scrollToPinnedEdge() {
  await pinnedEdgeScroller.scrollToPinnedEdge({ settle: true });
  updateCurrentDayMarker();
}

function newestRenderedMessage() {
  return orderedChildren.value[0]
    ?? activeEntry.value?.root
    ?? null;
}

function oldestRenderedMessage() {
  const children = orderedChildren.value;
  return children[children.length - 1]
    ?? activeEntry.value?.root
    ?? null;
}

function activeEdgeMessage(mode: ScrollDirectionMode) {
  return isTopPinnedScrollDirection(mode)
    ? newestRenderedMessage()
    : (orderedThreadMessages.value[orderedThreadMessages.value.length - 1] ?? oldestRenderedMessage());
}

function updateCurrentDayMarker() {
  const container = scrollContainerRef.value;
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

function setScrollContainerRef(el: HTMLElement | null) {
  scrollContainerRef.value = el;
  void nextTick(updateCurrentDayMarker);
}

const setVirtualTimelineRef = (instance: VirtualTimelineHandle | null) => {
  virtualTimelineRef.value = instance;
  setScrollContainerRef(instance?.scrollElement ?? null);
  virtualTimelineEdgeScroller.value = instance
    ? async (mode) => {
        const target = activeEdgeMessage(mode);
        if (!target) return false;
        return instance.scrollToMessageId(
          target.id,
          isTopPinnedScrollDirection(mode) ? "start" : "end",
        );
      }
    : null;
  void scrollToTargetMessage();
};

// Switching threads resets composer state and scrolls to the pinned edge.
watch(activeThreadId, () => {
  replyingTo.value = null;
  draft.value = "";
  if (props.targetMessageId) {
    void scrollToTargetMessage();
    return;
  }
  void scrollToPinnedEdge();
});

async function scrollToTargetMessage() {
  const targetMessageId = props.targetMessageId;
  if (
    !targetMessageId
    || !activeThreadId.value
    || !orderedThreadMessages.value.some((message) => message.id === targetMessageId)
  ) {
    return;
  }
  await nextTick();
  await virtualTimelineRef.value?.scrollToMessageId(targetMessageId, "center");
  updateCurrentDayMarker();
}

watch(
  [() => props.targetMessageId, activeThreadId, () => orderedThreadMessages.value.map((message) => message.id).join("\0")],
  () => {
    void scrollToTargetMessage();
  },
  { flush: "post" },
);

watch(scrollDirectionMode, () => {
  void scrollToPinnedEdge();
});

watch(
  () => props.isLoadingOlderReplies,
  (loading, wasLoading) => {
    if (loading) {
      pinnedEdgeScroller.disconnect();
      const el = scrollContainerRef.value;
      olderLoadRestoreToken++;
      olderLoadSnapshot = el
        ? {
            element: el,
            height: el.scrollHeight,
            mode: scrollDirectionMode.value,
            threadId: activeThreadId.value,
            top: el.scrollTop,
            token: olderLoadRestoreToken,
          }
        : null;
      return;
    }
    if (!wasLoading || !olderLoadSnapshot) return;
    const snapshot = olderLoadSnapshot;
    olderLoadSnapshot = null;
    void nextTick(() => {
      const el = scrollContainerRef.value;
      if (
        !props.isLoadingOlderReplies
        && snapshot.token === olderLoadRestoreToken
        && el === snapshot.element
        && activeThreadId.value === snapshot.threadId
        && scrollDirectionMode.value === snapshot.mode
        && !isTopPinnedScrollDirection(scrollDirectionMode.value)
      ) {
        el.scrollTop = snapshot.top + (el.scrollHeight - snapshot.height);
      }
      updateCurrentDayMarker();
    });
  },
);

watch(
  [activeEntry, orderedChildren],
  () => {
    void nextTick(updateCurrentDayMarker);
  },
  { flush: "post" },
);

watch(newestThreadMessageId, (newest, previousNewest) => {
  if (newest && previousNewest && newest !== previousNewest) {
    void scrollToPinnedEdge();
  }
});

onBeforeUnmount(() => {
  pinnedEdgeScroller.disconnect();
});

const breadcrumbLabels = computed(() =>
  props.threadStack.map((id) => {
    const entry = props.resolveEntry(id);
    const body = entry?.root?.body?.trim() ?? "";
    return body.length > 0 ? body.slice(0, 40) : id.slice(0, 8);
  }),
);

function hatsFor(author: string): OccupantHat[] {
  return props.roomHats[author] ?? [];
}

function presenceFor(author: string): OccupantPresence {
  return props.roomPresence[author] ?? "offline";
}

function beginReplyInThread(message: TimelineMessage) {
  if (props.hideComposer) return;
  replyingTo.value = {
    id: message.id,
    author: message.author,
    ...(message.body ? { body: message.body } : {}),
  };
  void nextTick(() => composerRef.value?.focus());
}

function cancelReplyInThread() {
  replyingTo.value = null;
}

function onSend(body: string, markup: MarkupSpan[], references: MessageReference[], files?: Array<File | Blob>) {
  const threadId = activeThreadId.value;
  if (!threadId) return;
  const entry = activeEntry.value;
  const root = entry?.root ?? null;
  const pending = replyingTo.value;
  // Default target = the thread root so the reply chip still points somewhere
  // useful; explicit in-thread reply overrides it.
  const effectiveReply = pending
    ? { id: pending.id, author: pending.author, ...(pending.body ? { body: pending.body } : {}) }
    : root
      ? { id: root.id, author: root.authorJid ?? root.author, ...(root.body ? { body: root.body } : {}) }
      : undefined;
  const override: { threadId: string; parentThreadId?: string } = { threadId };
  if (parentThreadId.value) override.parentThreadId = parentThreadId.value;
  emit("send", body, markup, references, files, effectiveReply, override);
  replyingTo.value = null;
  draft.value = "";
}

function onOpenThreadFromCard(threadId: string) {
  // Already-open thread id is a no-op; otherwise push it onto the stack.
  if (threadId === activeThreadId.value) return;
  emit("pushThread", threadId);
}

function replyChildHasNestedThread(message: TimelineMessage): boolean {
  const entry = props.threadIndex.get(message.id);
  return !!entry && entry.count > 0;
}
</script>

<template>
  <div class="flex flex-col flex-1 min-h-0 bg-background border-l border-border">
    <!-- Header: breadcrumb trail + close / back button -->
    <div class="chat-pane-header flex-shrink-0 flex items-center justify-between gap-3 border-b border-border px-4 py-0 glass-panel">
      <div class="type-control flex min-w-0 flex-1 items-center gap-1.5 truncate">
        <span class="text-muted-foreground">Thread</span>
        <template v-for="(label, i) in breadcrumbLabels" :key="i">
          <ChevronRight class="w-3 h-3 text-muted-foreground/60 flex-shrink-0" />
          <button
            type="button"
            class="truncate max-w-40 text-left hover:text-primary transition-colors"
            :class="i === breadcrumbLabels.length - 1 ? 'text-foreground' : 'text-muted-foreground'"
            :title="label"
            @click="emit('popTo', i)"
          >{{ label }}</button>
        </template>
      </div>
      <!-- Mobile back button: goes up one level in the thread stack (hidden on desktop) -->
      <button
        v-if="threadStack.length > 1"
        type="button"
        class="chat-icon-button flex-shrink-0 hover:bg-muted lg:hidden"
        title="Go back"
        aria-label="Go back"
        @click="emit('popTo', threadStack.length - 2)"
      >
        <ChevronLeft class="w-4 h-4" />
      </button>
      <!-- Close button: closes the entire thread panel -->
      <button
        type="button"
        class="chat-icon-button flex-shrink-0 hover:bg-muted"
        title="Close thread"
        aria-label="Close thread"
        @click="emit('close')"
      >
        <X class="w-4 h-4" />
      </button>
    </div>

    <!-- Composer: top-pinned mode (social mode) -->
    <div v-if="!hideComposer && isTopPinned" class="flex-shrink-0">
      <MessageComposer
        :ref="setComposerRef"
        v-model:draft="draft"
        :channel-name="`thread in ${channelName}`"
        :is-sending="isSending"
        :disabled="false"
        :giphy-api-key="giphyApiKey"
        :mention-candidates="mentionCandidates"
        :slow-mode-cooldown="slowModeCooldown"
        :upload-progress="uploadProgress"
        :replying-to="replyingTo"
        :is-top-pinned="true"
        @send="onSend"
        @cancel-reply="cancelReplyInThread"
        @typing="emit('typing')"
        @select-gif="(url: string) => emit('selectGif', url)"
      />
    </div>

    <!-- Messages scroll area -->
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

    <div
      v-if="activeEntry && !activeEntry.root"
      class="type-caption flex-shrink-0 bg-muted/35 px-4 py-2 text-muted-foreground"
    >
      Thread root isn't in the loaded history. Scroll the main channel or reload to backfill.
    </div>

    <VirtualTimeline
      v-if="activeEntry"
      :ref="setVirtualTimelineRef"
      :items="orderedThreadMessages"
      :has-older="hasOlderReplies ?? false"
      :loading-older="isLoadingOlderReplies ?? false"
      :sentinel-position="olderSentinelPosition"
      aria-label="Thread messages"
      content-class="chat-panel-stack"
      @scroll="updateCurrentDayMarker"
      @load-older="activeThreadId && emit('loadOlder', activeThreadId)"
    >
      <template #item="{ item: message }">
        <template v-if="message.id === activeEntry.root?.id">
          <div
            v-if="showDayDividerBefore(message.id)"
            class="chat-day-divider type-section-label"
            :data-day-marker-created-at="message.createdAt"
            role="separator"
            :aria-label="dayDividerLabel(message.createdAt)"
          >
            <div class="chat-day-divider__rule" />
            <span class="chat-day-divider__label">{{ dayDividerLabel(message.createdAt) }}</span>
            <div class="chat-day-divider__rule" />
          </div>
          <div class="chat-thread-root" aria-label="Thread root message">
            <span class="chat-thread-root__label type-section-label">Thread start</span>
            <MessageCard
              :message="message"
              :current-user="currentUser"
              :current-user-jid="currentUserJid"
              :avatar-url="avatarUrlByAuthor[message.author] ?? null"
              :hats="hatsFor(message.author)"
              :presence="presenceFor(message.author)"
              :last-seen="roomLastSeen[message.author]"
              :author-jid="authorJidByNick?.[message.author]"
              :thread-reply-count="activeEntry.count"
              hide-thread-chip
              hide-reply-chip
              :reaction-mode-selected="reactionMode?.selectedMessageId === message.id"
              :invoke-extension-action="props.invokeExtensionAction"
              @edit="(id, body, m, r) => emit('editMessage', id, body, m, r)"
              @retract="(id) => emit('retractMessage', id)"
              @react="(id, emoji) => emit('reactMessage', id, emoji)"
              @reply="beginReplyInThread"
              @open-thread="onOpenThreadFromCard"
            />
          </div>
        </template>
        <template v-else>
          <div
            v-if="showDayDividerBefore(message.id)"
            class="chat-day-divider type-section-label"
            :data-day-marker-created-at="message.createdAt"
            role="separator"
            :aria-label="dayDividerLabel(message.createdAt)"
          >
            <div class="chat-day-divider__rule" />
            <span class="chat-day-divider__label">{{ dayDividerLabel(message.createdAt) }}</span>
            <div class="chat-day-divider__rule" />
          </div>
          <div class="relative group/thread-child">
            <MessageCard
              :message="message"
              :current-user="currentUser"
              :current-user-jid="currentUserJid"
              :avatar-url="avatarUrlByAuthor[message.author] ?? null"
              :hats="hatsFor(message.author)"
              :presence="presenceFor(message.author)"
              :last-seen="roomLastSeen[message.author]"
              :author-jid="authorJidByNick?.[message.author]"
              :thread-reply-count="threadIndex.get(message.id)?.count ?? 0"
              :grouped="isGroupedFollowUp(message.id)"
              hide-thread-chip
              hide-reply-chip
              :reaction-mode-selected="reactionMode?.selectedMessageId === message.id"
              :invoke-extension-action="props.invokeExtensionAction"
              @edit="(id, body, m, r) => emit('editMessage', id, body, m, r)"
              @retract="(id) => emit('retractMessage', id)"
              @react="(id, emoji) => emit('reactMessage', id, emoji)"
              @reply="beginReplyInThread"
              @open-thread="onOpenThreadFromCard"
            />
          <div v-if="replyChildHasNestedThread(message)" class="chat-thread-actions">
            <button
              type="button"
              class="type-caption inline-flex items-center gap-1 text-primary/80 hover:text-primary transition-colors"
              @click="onOpenThreadFromCard(message.id)"
            >
              <CornerDownRight class="w-3 h-3" />
              <span>{{ threadIndex.get(message.id)?.count ?? 0 }} in sub-thread</span>
            </button>
          </div>
          </div>
        </template>
      </template>
    </VirtualTimeline>

    <div
      v-else
      :ref="setScrollContainerRef"
      class="chat-pane-scroll flex-1 min-h-0 px-3 py-4 lg:px-4"
      @scroll="updateCurrentDayMarker"
    >
      <div class="type-caption text-center py-10 text-muted-foreground">
        Loading thread…
      </div>
    </div>

    <div
      v-if="activeEntry?.directChildren.length === 0 && !hideComposer"
      class="type-caption flex-shrink-0 text-center py-3 text-muted-foreground"
    >
      No replies yet. Start the conversation.
    </div>

    <!-- Composer: bottom-pinned mode (chat mode) - hidden in parent context pane -->
    <div v-if="!hideComposer && !isTopPinned" class="flex-shrink-0">
      <MessageComposer
        :ref="setComposerRef"
        v-model:draft="draft"
        :channel-name="`thread in ${channelName}`"
        :is-sending="isSending"
        :disabled="false"
        :giphy-api-key="giphyApiKey"
        :mention-candidates="mentionCandidates"
        :slow-mode-cooldown="slowModeCooldown"
        :upload-progress="uploadProgress"
        :replying-to="replyingTo"
        :is-top-pinned="false"
        @send="onSend"
        @cancel-reply="cancelReplyInThread"
        @typing="emit('typing')"
        @select-gif="(url: string) => emit('selectGif', url)"
      />
    </div>
  </div>
</template>
