<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { X, CornerDownRight, MessageSquarePlus, ChevronRight, ChevronLeft } from "lucide-vue-next";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import type { TimelineMessage, MarkupSpan, MessageReference } from "@/lib/chat-ui";
import type { MentionCandidate } from "@/lib/mentions";
import type { OccupantHat, OccupantPresence, RoomHats, RoomPresence } from "@/lib/xmpp-client";
import type { ThreadEntry, ThreadIndex } from "@/composables/useThreads";
import { useScrollDirection } from "@/composables/useScrollDirection";
import { isTopPinnedScrollDirection, orderTimelineForScrollDirection, getPinnedScrollTop } from "@/lib/scroll-direction";
import { formatDayDivider, isSameDay } from "@/composables/useMessaging";

const props = defineProps<{
  threadStack: string[];
  threadIndex: ThreadIndex;
  /**
   * Resolves a thread entry, synthesising an empty one when the id points
   * at a known message with no replies yet (e.g. a freshly-started
   * sub-thread). Returns undefined when the root hasn't been loaded.
   */
  resolveEntry: (threadId: string) => ThreadEntry | undefined;
  currentUser?: string;
  avatarUrlByAuthor: Record<string, string | null>;
  authorJidByNick?: Record<string, string>;
  roomHats: RoomHats;
  roomPresence: RoomPresence;
  roomLastSeen: Record<string, number>;
  tenorApiKey: string;
  mentionCandidates: MentionCandidate[];
  slowModeCooldown: number;
  isSending: boolean;
  uploadProgress: { uploading: boolean; progress: number; filename: string };
  channelName: string;
  /**
   * When true, the composer is hidden but sub-thread navigation stays active.
   * Used to render the parent context pane in the accordion layout.
   */
  hideComposer?: boolean;
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
}>();

const { mode: scrollDirectionMode } = useScrollDirection();

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

// Burst window matches the main feed (ContentArea.vue): same author + < 5 min
// apart + same day, no intervening other-author message in rendered order.
const BURST_WINDOW_MS = 5 * 60 * 1000;

const threadDisplayMeta = computed(() => {
  const grouped = new Set<string>();
  const dayDivider = new Set<string>();
  const root = activeEntry.value?.root;
  const sequence: TimelineMessage[] = root ? [root, ...orderedChildren.value] : [...orderedChildren.value];
  for (let i = 0; i < sequence.length; i++) {
    const cur = sequence[i];
    if (!cur) continue;
    const prev = i > 0 ? sequence[i - 1] : null;
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
  return threadDisplayMeta.value.grouped.has(messageId);
}

function showDayDividerBefore(messageId: string): boolean {
  return threadDisplayMeta.value.dayDivider.has(messageId);
}

function dayDividerLabel(createdAt: string): string {
  return formatDayDivider(createdAt);
}

const isTopPinned = computed(() =>
  isTopPinnedScrollDirection(scrollDirectionMode.value),
);

const scrollContainerRef = ref<HTMLElement | null>(null);
const currentDayMarkerLabel = ref("");
const draft = ref("");
const replyingTo = ref<{ id: string; author: string; body?: string } | null>(null);

type ComposerHandle = { focus: () => void };
const composerRef = ref<ComposerHandle | null>(null);

function setComposerRef(el: ComposerHandle | null) {
  composerRef.value = el;
}

async function scrollToPinnedEdge() {
  // Two ticks: first lets Vue flush the DOM update, second gives the browser
  // time to recalculate layout (scrollHeight) before we set scrollTop.
  await nextTick();
  await nextTick();
  const el = scrollContainerRef.value;
  if (!el) return;
  el.scrollTop = getPinnedScrollTop(el, scrollDirectionMode.value);
  updateCurrentDayMarker();
}

function updateCurrentDayMarker() {
  const container = scrollContainerRef.value;
  if (!container) {
    currentDayMarkerLabel.value = "";
    return;
  }

  const messageEls = [...container.querySelectorAll<HTMLElement>("[data-message-created-at]")];
  if (messageEls.length === 0) {
    currentDayMarkerLabel.value = "";
    return;
  }

  const containerTop = container.getBoundingClientRect().top;
  const probeTop = containerTop + 1;
  let current = messageEls[0];
  for (const el of messageEls) {
    const rect = el.getBoundingClientRect();
    if (rect.bottom < probeTop) {
      current = el;
      continue;
    }
    if (rect.top <= probeTop || current === messageEls[0]) {
      current = el;
    }
    break;
  }

  const createdAt = current.dataset.messageCreatedAt;
  currentDayMarkerLabel.value = createdAt ? formatDayDivider(createdAt) : "";
}

// Switching threads resets composer state and scrolls to the pinned edge.
watch(activeThreadId, () => {
  replyingTo.value = null;
  draft.value = "";
  void scrollToPinnedEdge();
});

watch(
  () => activeEntry.value?.directChildren.length,
  () => {
    void scrollToPinnedEdge();
  },
);

watch(
  [activeEntry, orderedChildren],
  () => {
    void nextTick(updateCurrentDayMarker);
  },
  { flush: "post" },
);

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

function startSubThread(message: TimelineMessage) {
  emit("pushThread", message.id);
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
        :tenor-api-key="tenorApiKey"
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
      ref="scrollContainerRef"
      class="chat-pane-scroll flex-1 min-h-0 overflow-auto px-3 py-4 lg:px-4"
      @scroll="updateCurrentDayMarker"
    >
      <div v-if="activeEntry" class="chat-panel-stack">
        <div v-if="!activeEntry.root" class="type-caption rounded-lg bg-muted/40 px-3 py-2 text-muted-foreground">
          Thread root isn't in the loaded history. Scroll the main channel or reload to backfill.
        </div>
        <MessageCard
          v-if="activeEntry.root"
          :message="activeEntry.root"
          :current-user="currentUser"
          :avatar-url="avatarUrlByAuthor[activeEntry.root.author] ?? null"
          :hats="hatsFor(activeEntry.root.author)"
          :presence="presenceFor(activeEntry.root.author)"
          :last-seen="roomLastSeen[activeEntry.root.author]"
          :author-jid="authorJidByNick?.[activeEntry.root.author]"
          :thread-reply-count="activeEntry.count"
          hide-thread-chip
          @edit="(id, body, m, r) => emit('editMessage', id, body, m, r)"
          @retract="(id) => emit('retractMessage', id)"
          @react="(id, emoji) => emit('reactMessage', id, emoji)"
          @reply="beginReplyInThread"
          @open-thread="onOpenThreadFromCard"
        />

        <template v-for="child in orderedChildren" :key="child.id">
          <div
            v-if="showDayDividerBefore(child.id)"
            class="chat-day-divider type-section-label"
            role="separator"
            :aria-label="dayDividerLabel(child.createdAt)"
          >
            <div class="chat-day-divider__rule" />
            <span class="chat-day-divider__label">{{ dayDividerLabel(child.createdAt) }}</span>
            <div class="chat-day-divider__rule" />
          </div>
          <div class="relative group/thread-child">
            <MessageCard
              :message="child"
              :current-user="currentUser"
              :avatar-url="avatarUrlByAuthor[child.author] ?? null"
              :hats="hatsFor(child.author)"
              :presence="presenceFor(child.author)"
              :last-seen="roomLastSeen[child.author]"
              :author-jid="authorJidByNick?.[child.author]"
              :thread-reply-count="threadIndex.get(child.id)?.count ?? 0"
              :grouped="isGroupedFollowUp(child.id)"
              hide-thread-chip
              @edit="(id, body, m, r) => emit('editMessage', id, body, m, r)"
              @retract="(id) => emit('retractMessage', id)"
              @react="(id, emoji) => emit('reactMessage', id, emoji)"
              @reply="beginReplyInThread"
              @open-thread="onOpenThreadFromCard"
            />
          <div class="chat-thread-actions">
            <button
              v-if="replyChildHasNestedThread(child)"
              type="button"
              class="type-caption inline-flex items-center gap-1 text-primary/80 hover:text-primary transition-colors"
              @click="onOpenThreadFromCard(child.id)"
            >
              <CornerDownRight class="w-3 h-3" />
              <span>{{ threadIndex.get(child.id)?.count ?? 0 }} in sub-thread</span>
            </button>
            <button
              v-else
              type="button"
              class="type-caption inline-flex items-center gap-1 text-muted-foreground hover:text-primary transition-colors opacity-60 group-hover/thread-child:opacity-100 focus-visible:opacity-100"
              title="Start sub-thread"
              @click="startSubThread(child)"
            >
              <MessageSquarePlus class="w-3 h-3" />
              <span>Start sub-thread</span>
            </button>
          </div>
          </div>
        </template>

        <div
          v-if="activeEntry.directChildren.length === 0 && !hideComposer"
          class="type-caption text-center py-8 text-muted-foreground"
        >
          No replies yet. Start the conversation.
        </div>
      </div>
      <div
        v-else
        class="type-caption text-center py-10 text-muted-foreground"
      >
        Loading thread…
      </div>
    </div>

    <!-- Composer: bottom-pinned mode (chat mode) - hidden in parent context pane -->
    <div v-if="!hideComposer && !isTopPinned" class="flex-shrink-0">
      <MessageComposer
        :ref="setComposerRef"
        v-model:draft="draft"
        :channel-name="`thread in ${channelName}`"
        :is-sending="isSending"
        :disabled="false"
        :tenor-api-key="tenorApiKey"
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
