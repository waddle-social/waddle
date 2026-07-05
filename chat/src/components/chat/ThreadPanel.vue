<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch, type ComponentPublicInstance, type Ref } from "vue";
import { ArrowDown, ArrowUp, CornerDownRight } from "lucide-vue-next";
import { useJumpToLiveEdge } from "@/ui/use-jump-to-live-edge";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import ThreadPanelHeader from "@/components/chat/ThreadPanelHeader.vue";
import VirtualTimeline from "@/components/chat/VirtualTimeline.vue";
import type { ExtensionAnnotationAction, TimelineMessage, MarkupSpan, MessageReference } from "@/lib/chat-ui";
import type { MentionCandidate } from "@/lib/mentions";
import type { ComposerLinkPreviewLookup, ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
import type { OccupantAuthority, OccupantHat, OccupantPresence, RoomAuthority, RoomHats, RoomPresence } from "@/lib/xmpp-client";
import type { MessageThreadEntry, MessageThreadIndex } from "@/channels/threads";
import { createScrollFrameScheduler } from "@/ui/scroll-frame";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import { isTopPinnedScrollDirection, type ScrollDirectionMode } from "@/lib/scroll-direction";
import { latestRemoteMessageIdFor } from "@/lib/timeline-state";
import { createPinnedEdgeScroller } from "@/lib/pinned-edge-scroll";
import { useChatWindowVisibility } from "@/shell/window-visibility";
import { formatTimelineDayDivider } from "@/channels/timeline";
import { useCallAnchorCardState } from "@/lib/call-thread-anchor";
import { buildReplyChildThreadTargets, resolveReplyChildThreadTarget } from "@/lib/thread-child-target";
import type { CallMedia } from "@/lib/calls/types";
import { currentDayMarkerLabelFor } from "./current-day-marker";
import { buildMessageDisplayMeta } from "./timeline-display-meta";
import {
  chronologicalThreadReplies,
  newestThreadMessageId as newestThreadMessageIdFor,
  olderRepliesSentinelPosition,
  orderThreadChildren,
  orderThreadMessages,
  threadEdgeMessage,
} from "./thread-panel-messages";
import {
  formatThreadLastActivity,
  threadBreadcrumbLabels,
  threadLastActivityFor,
  threadParticipantsFor,
  threadPreviewFor,
} from "./thread-lobby-meta";
import { useThreadComposer } from "./composables/use-thread-composer";
import { useThreadDisplayedTracking } from "./composables/use-thread-displayed-tracking";
import { useThreadOlderLoadRestore } from "./composables/use-thread-older-load-restore";
import { useThreadTargetScroll } from "./composables/use-thread-target-scroll";

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
  roomAuthority: RoomAuthority;
  roomPresence: RoomPresence;
  roomLastSeen: Record<string, number>;
  mentionCandidates: MentionCandidate[];
  slowModeCooldown: number;
  isSending: boolean;
  isLoadingOlderReplies?: boolean;
  hasOlderReplies?: boolean;
  uploadProgress: { uploading: boolean; progress: number; filename: string };
  channelName: string;
  channelId?: string | null;
  roomJid?: string | null;
  reactionMode?: { selectedMessageId: string | null } | null;
  targetMessageId?: string | null;
  targetMessageRequestId?: number;
  /**
   * When true, the composer is hidden but sub-thread navigation stays active.
   * Used to render the parent context pane in the accordion layout.
   */
  hideComposer?: boolean;
  invokeExtensionAction?: (action: ExtensionAnnotationAction) => Promise<unknown>;
  linkPreviewLookup?: ComposerLinkPreviewLookup | null;
  linkPreviewScope?: string | null;
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
    linkPreview?: ComposerLinkPreviewSendPayload,
  ];
  editMessage: [messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[], linkPreview?: ComposerLinkPreviewSendPayload];
  retractMessage: [messageId: string];
  reactMessage: [messageId: string, emoji: string];
  displayed: [messageId: string, options?: { syncMds?: boolean }];
  selectGif: [
    url: string,
    threadOverride: { threadId: string; parentThreadId?: string },
  ];
  // Typing notifications from inside a thread carry the active thread so
  // the outbound XEP-0085 chat-state can include an XEP-0201 `<thread/>`
  // and peers can show "typing in thread X" instead of channel-wide.
  typing: [threadOverride?: { threadId: string; parentThreadId?: string }];
  loadOlder: [threadId: string];
  joinChannelCall: [channelId: string | null, roomJid: string, media: CallMedia];
}>();

const { mode: scrollDirectionMode } = useScrollDirectionPreference();

const activeThreadId = computed(() => props.threadStack[props.threadStack.length - 1] ?? null);
const parentThreadId = computed(() =>
  props.threadStack.length >= 2 ? props.threadStack[props.threadStack.length - 2] : undefined,
);
const activeEntry = computed(() =>
  activeThreadId.value ? props.resolveEntry(activeThreadId.value) ?? null : null,
);

const orderedChildren = computed(() =>
  orderThreadChildren(activeEntry.value, scrollDirectionMode.value),
);
const orderedThreadMessages = computed(() =>
  orderThreadMessages(activeEntry.value, orderedChildren.value, scrollDirectionMode.value),
);
const newestThreadMessageId = computed(() => newestThreadMessageIdFor(activeEntry.value));
const replyChildThreadTargets = computed(() =>
  buildReplyChildThreadTargets(props.threadIndex, activeThreadId.value),
);
const latestRemoteThreadMessageId = computed(() =>
  latestRemoteMessageIdFor(chronologicalThreadReplies(activeEntry.value, activeThreadId.value)),
);

// Burst grouping and day dividers match the main feed (ContentArea.vue).
const threadDisplayMeta = computed(() => buildMessageDisplayMeta(orderedThreadMessages.value));

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
const olderSentinelPosition = computed(() =>
  olderRepliesSentinelPosition(scrollDirectionMode.value, Boolean(activeEntry.value?.root)),
);

const scrollContainerRef = ref<HTMLElement | null>(null);
const virtualTimelineEdgeScroller: Ref<((mode: ScrollDirectionMode) => boolean | Promise<boolean>) | null> = ref(null);
const pinnedEdgeScroller = createPinnedEdgeScroller({
  element: scrollContainerRef,
  mode: scrollDirectionMode,
  virtualScroll: virtualTimelineEdgeScroller,
});
const currentDayMarkerLabel = ref("");

const {
  draft,
  replyingTo,
  setComposerRef,
  beginReplyInThread,
  cancelReplyInThread,
  resetForThreadSwitch,
  onSend,
  onSelectGif,
  onTyping,
} = useThreadComposer({
  activeThreadId: () => activeThreadId.value,
  parentThreadId: () => parentThreadId.value,
  rootMessage: () => activeEntry.value?.root ?? null,
  hideComposer: () => props.hideComposer ?? false,
  emitSend: (body, markup, references, files, replyTo, threadOverride, linkPreview) =>
    emit("send", body, markup, references, files, replyTo, threadOverride, linkPreview),
  emitSelectGif: (url, threadOverride) => emit("selectGif", url, threadOverride),
  emitTyping: (threadOverride) => emit("typing", threadOverride),
});

type VirtualTimelineHandle = ComponentPublicInstance & {
  scrollElement: HTMLDivElement | null;
  scrollToMessageId: (messageId: string, align?: "start" | "center" | "end") => Promise<boolean>;
};
const virtualTimelineRef = ref<VirtualTimelineHandle | null>(null);

// "Jump to latest" floating button — same composable as ContentArea
// so the affordance is consistent between channel and thread surfaces.
// Live edge flips with scroll-direction preference (top in social,
// bottom in chat). The pinned-edge scroller already knows the mode,
// so the scrollToEdge callback just delegates.
const jumpToLive = useJumpToLiveEdge({
  scrollElement: computed(() => virtualTimelineRef.value?.scrollElement ?? null),
  mode: scrollDirectionMode,
  scrollToEdge: () => {
    void scrollToPinnedEdge();
    return true;
  },
});
const threadScrollFrame = createScrollFrameScheduler(() => {
  updateCurrentDayMarker();
  jumpToLive.updateDistance();
});

function onThreadScroll() {
  threadScrollFrame.schedule();
}

async function scrollToPinnedEdge() {
  await pinnedEdgeScroller.scrollToPinnedEdge({ settle: true });
  updateCurrentDayMarker();
}

function updateCurrentDayMarker() {
  currentDayMarkerLabel.value = currentDayMarkerLabelFor(scrollContainerRef.value);
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
        const target = threadEdgeMessage(
          orderedChildren.value,
          orderedThreadMessages.value,
          activeEntry.value?.root ?? null,
          mode,
        );
        if (!target) return false;
        return instance.scrollToMessageId(
          target.id,
          isTopPinnedScrollDirection(mode) ? "start" : "end",
        );
      }
    : null;
  void targetScroll.scrollToTargetMessage();
};

// Switching threads resets composer state and scrolls to the pinned edge.
watch(activeThreadId, () => {
  resetForThreadSwitch();
  if (props.targetMessageId) {
    void targetScroll.scrollToTargetMessage();
    return;
  }
  targetScroll.cancelPendingTargetScroll();
  void scrollToPinnedEdge();
});

const targetScroll = useThreadTargetScroll({
  targetMessageId: () => props.targetMessageId,
  targetMessageRequestId: () => props.targetMessageRequestId ?? 0,
  activeThreadId: () => activeThreadId.value,
  orderedMessages: () => orderedThreadMessages.value,
  timeline: () => virtualTimelineRef.value,
  afterScroll: () => {
    pinnedEdgeScroller.refreshPinnedState();
    updateCurrentDayMarker();
  },
  markDisplayed: () => displayedTracking.markThreadDisplayedIfVisible(),
});

watch(scrollDirectionMode, () => {
  if (targetScroll.targetScrollPending.value) return;
  void scrollToPinnedEdge();
});

// Browser-tab refocus: re-pin the thread panel if the user was already at
// the edge before switching away. Mirrors the channel/DM message-list
// watchers; see `channels/messages.ts` / `dms/messages.ts`.
const { isWindowFocused } = useChatWindowVisibility();
watch(isWindowFocused, (focused, prev) => {
  if (focused && !prev && !targetScroll.targetScrollPending.value && pinnedEdgeScroller.isPinnedAtEdge.value) {
    void scrollToPinnedEdge();
  }
});

const displayedTracking = useThreadDisplayedTracking({
  activeThreadId: () => activeThreadId.value,
  latestRemoteMessageId: () => latestRemoteThreadMessageId.value,
  hideComposer: () => props.hideComposer ?? false,
  targetScrollPending: () => targetScroll.targetScrollPending.value,
  scrollContainer: scrollContainerRef,
  isWindowFocused: () => isWindowFocused.value,
  isPinnedAtEdge: () => pinnedEdgeScroller.isPinnedAtEdge.value,
  chatKey: () => props.roomJid ?? props.linkPreviewScope ?? props.channelId ?? props.channelName,
  emitDisplayed: (messageId) => emit("displayed", messageId, { syncMds: false }),
});

useThreadOlderLoadRestore({
  isLoadingOlderReplies: () => props.isLoadingOlderReplies,
  scrollContainer: scrollContainerRef,
  mode: () => scrollDirectionMode.value,
  activeThreadId: () => activeThreadId.value,
  cancelSettleLock: () => pinnedEdgeScroller.cancelSettleLock(),
  afterRestore: updateCurrentDayMarker,
});

watch(
  [activeEntry, orderedChildren],
  () => {
    void nextTick(updateCurrentDayMarker);
  },
  { flush: "post" },
);

watch(newestThreadMessageId, (newest, previousNewest) => {
  if (
    newest
    && previousNewest
    && newest !== previousNewest
    && !targetScroll.targetScrollPending.value
    && pinnedEdgeScroller.isPinnedAtEdge.value
  ) {
    void scrollToPinnedEdge();
  }
});

onBeforeUnmount(() => {
  threadScrollFrame.disconnect();
  pinnedEdgeScroller.disconnect();
});

// Thread "lobby" metadata for the rich header (see `thread-lobby-meta.ts`).
const breadcrumbLabels = computed(() =>
  threadBreadcrumbLabels(props.threadStack, props.resolveEntry),
);
const threadReplyCount = computed(() => activeEntry.value?.count ?? 0);
const threadCallAnchorState = useCallAnchorCardState(
  () => activeEntry.value?.root ?? { body: "", author: "" },
  () => props.roomJid,
  () => threadReplyCount.value,
);
const threadRootAuthor = computed(() => activeEntry.value?.root?.author ?? null);
const threadPreview = computed(() => threadPreviewFor(activeEntry.value));
const threadParticipants = computed(() =>
  threadParticipantsFor(activeEntry.value, props.avatarUrlByAuthor, props.roomPresence),
);
const threadLastActivityLabel = computed(() =>
  formatThreadLastActivity(threadLastActivityFor(activeEntry.value)),
);

function hatsFor(author: string): OccupantHat[] {
  return props.roomHats[author] ?? [];
}

function authorityFor(author: string): OccupantAuthority | null {
  return props.roomAuthority[author] ?? null;
}

function presenceFor(author: string): OccupantPresence {
  return props.roomPresence[author] ?? "offline";
}

function onOpenThreadFromCard(threadId: string) {
  // Already-open thread id is a no-op; otherwise push it onto the stack.
  if (threadId === activeThreadId.value) return;
  emit("pushThread", threadId);
}

function joinThreadCallAnchor() {
  const state = threadCallAnchorState.value;
  if (!state || !props.roomJid || state.status !== "live") return;
  emit("joinChannelCall", props.channelId ?? null, props.roomJid, state.media);
}

function replyChildThreadTarget(message: TimelineMessage) {
  return resolveReplyChildThreadTarget(replyChildThreadTargets.value, message);
}

function replyChildThreadCount(message: TimelineMessage): number {
  return replyChildThreadTarget(message).count;
}

function replyChildThreadId(message: TimelineMessage): string {
  return replyChildThreadTarget(message).threadId;
}

function replyChildHasNestedThread(message: TimelineMessage): boolean {
  return replyChildThreadCount(message) > 0;
}
</script>

<template>
  <div class="flex flex-col flex-1 min-h-0 bg-background border-l border-border">
    <ThreadPanelHeader
      :breadcrumb-labels="breadcrumbLabels"
      :thread-preview="threadPreview"
      :call-anchor-state="threadCallAnchorState"
      :root-author="threadRootAuthor"
      :reply-count="threadReplyCount"
      :last-activity-label="threadLastActivityLabel"
      :participants="threadParticipants"
      @close="emit('close')"
      @pop-to="(index) => emit('popTo', index)"
      @join-call="joinThreadCallAnchor"
      @open-thread="onOpenThreadFromCard"
    />

    <!-- Composer: top-pinned mode (social mode) -->
    <div v-if="!hideComposer && isTopPinned" class="flex-shrink-0">
      <MessageComposer
        :ref="setComposerRef"
        v-model:draft="draft"
        :channel-name="`thread in ${channelName}`"
        :is-sending="isSending"
        :disabled="false"
        :mention-candidates="mentionCandidates"
        :slow-mode-cooldown="slowModeCooldown"
        :upload-progress="uploadProgress"
        :replying-to="replyingTo"
        :is-top-pinned="true"
        :link-preview-lookup="linkPreviewLookup"
        :link-preview-scope="linkPreviewScope"
        @send="onSend"
        @cancel-reply="cancelReplyInThread"
        @typing="onTyping"
        @select-gif="onSelectGif"
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

    <div v-if="activeEntry" class="chat-message-pane">
    <VirtualTimeline
      :ref="setVirtualTimelineRef"
      :items="orderedThreadMessages"
      :has-older="hasOlderReplies ?? false"
      :loading-older="isLoadingOlderReplies ?? false"
      :sentinel-position="olderSentinelPosition"
      aria-label="Thread messages"
      content-class="chat-panel-stack"
      @scroll="onThreadScroll"
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
          <!-- Thread root message — rendered as a normal MessageCard.
               The thread-lobby header at the top of the panel already
               identifies who started the thread ("Started by …"), so
               wrapping the root in a primary-tinted card with a loud
               THREAD START label was redundant chrome. The replies
               that follow are visually separated by a hairline
               `.chat-thread-root-divider` instead. -->
          <MessageCard
            :message="message"
            :current-user="currentUser"
            :current-user-jid="currentUserJid"
            :avatar-url="avatarUrlByAuthor[message.author] ?? null"
            :hats="hatsFor(message.author)"
            :authority="authorityFor(message.author)"
            :presence="presenceFor(message.author)"
            :last-seen="roomLastSeen[message.author]"
            :author-jid="authorJidByNick?.[message.author]"
            :thread-reply-count="activeEntry.count"
            hide-thread-chip
            hide-reply-chip
            :reaction-mode-selected="reactionMode?.selectedMessageId === message.id"
            :invoke-extension-action="props.invokeExtensionAction"
            :link-preview-lookup="props.linkPreviewLookup"
            :link-preview-scope="props.linkPreviewScope"
            :thread-action-thread-id="activeThreadId ?? message.id"
            :call-room-jid="props.roomJid ?? null"
            :call-channel-id="props.channelId ?? null"
            :hide-call-anchor-card="!!threadCallAnchorState"
            @edit="(id, body, m, r, lp) => emit('editMessage', id, body, m, r, lp)"
            @retract="(id) => emit('retractMessage', id)"
            @react="(id, emoji) => emit('reactMessage', id, emoji)"
            @reply="beginReplyInThread"
            @open-thread="onOpenThreadFromCard"
            @join-channel-call="(channelId, roomJid, media) => emit('joinChannelCall', channelId, roomJid, media)"
          />
          <div
            v-if="orderedChildren.length > 0"
            class="chat-thread-root-divider"
            role="separator"
            aria-label="Replies"
          />
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
            <!-- In ThreadPanel, the card's thread action targets an existing
                 XEP-0201 child thread when present, falling back to this row's
                 message id to start an empty child thread. -->
            <MessageCard
              :message="message"
              :current-user="currentUser"
              :current-user-jid="currentUserJid"
              :avatar-url="avatarUrlByAuthor[message.author] ?? null"
              :hats="hatsFor(message.author)"
              :authority="authorityFor(message.author)"
              :presence="presenceFor(message.author)"
              :last-seen="roomLastSeen[message.author]"
              :author-jid="authorJidByNick?.[message.author]"
              :thread-reply-count="replyChildThreadCount(message)"
              :grouped="isGroupedFollowUp(message.id)"
              hide-thread-chip
              hide-reply-chip
              :reaction-mode-selected="reactionMode?.selectedMessageId === message.id"
              :invoke-extension-action="props.invokeExtensionAction"
              :link-preview-lookup="props.linkPreviewLookup"
              :link-preview-scope="props.linkPreviewScope"
              :thread-action-thread-id="replyChildThreadId(message)"
              :call-room-jid="props.roomJid ?? null"
              :call-channel-id="props.channelId ?? null"
              @edit="(id, body, m, r, lp) => emit('editMessage', id, body, m, r, lp)"
              @retract="(id) => emit('retractMessage', id)"
              @react="(id, emoji) => emit('reactMessage', id, emoji)"
              @reply="beginReplyInThread"
              @open-thread="onOpenThreadFromCard"
              @join-channel-call="(channelId, roomJid, media) => emit('joinChannelCall', channelId, roomJid, media)"
            />
          <div v-if="replyChildHasNestedThread(message)" class="chat-thread-actions">
            <button
              type="button"
              class="type-caption inline-flex items-center gap-1 text-primary/80 hover:text-primary transition-colors"
              @click="onOpenThreadFromCard(replyChildThreadId(message))"
            >
              <CornerDownRight class="w-3 h-3" />
              <span>{{ replyChildThreadCount(message) }} in sub-thread</span>
            </button>
          </div>
          </div>
        </template>
      </template>
    </VirtualTimeline>
    <button
      v-if="jumpToLive.shouldShow.value"
      type="button"
      class="chat-jump-to-live"
      :class="isTopPinned ? 'chat-jump-to-live--top' : 'chat-jump-to-live--bottom'"
      title="Jump to latest"
      aria-label="Jump to latest message"
      @click="jumpToLive.jump"
    >
      <ArrowUp v-if="isTopPinned" class="w-3.5 h-3.5" aria-hidden="true" />
      <ArrowDown v-else class="w-3.5 h-3.5" aria-hidden="true" />
      <span>Latest</span>
    </button>
    </div>

    <div
      v-else
      :ref="setScrollContainerRef"
      class="chat-pane-scroll flex-1 min-h-0 px-3 py-4 lg:px-4"
      @scroll="onThreadScroll"
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
        :mention-candidates="mentionCandidates"
        :slow-mode-cooldown="slowModeCooldown"
        :upload-progress="uploadProgress"
        :replying-to="replyingTo"
        :is-top-pinned="false"
        :link-preview-lookup="linkPreviewLookup"
        :link-preview-scope="linkPreviewScope"
        @send="onSend"
        @cancel-reply="cancelReplyInThread"
        @typing="onTyping"
        @select-gif="onSelectGif"
      />
    </div>
  </div>
</template>
