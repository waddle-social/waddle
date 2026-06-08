<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch, type ComponentPublicInstance, type Ref } from "vue";
import { ArrowDown, ArrowUp, X, CornerDownRight, ChevronRight, ChevronLeft } from "lucide-vue-next";
import { useJumpToLiveEdge } from "@/ui/use-jump-to-live-edge";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import CallAnchorCard from "@/components/calls/CallAnchorCard.vue";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import VirtualTimeline from "@/components/chat/VirtualTimeline.vue";
import type { ExtensionAnnotationAction, TimelineMessage, MarkupSpan, MessageReference } from "@/lib/chat-ui";
import type { MentionCandidate } from "@/lib/mentions";
import type { ComposerLinkPreviewLookup, ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
import type { OccupantAuthority, OccupantHat, OccupantPresence, RoomAuthority, RoomHats, RoomPresence } from "@/lib/xmpp-client";
import type { MessageThreadEntry, MessageThreadIndex } from "@/channels/threads";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import {
  isTopPinnedScrollDirection,
  orderTimelineForScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";
import { createPinnedEdgeScroller } from "@/lib/pinned-edge-scroll";
import { useChatWindowVisibility } from "@/shell/window-visibility";
import { formatTimelineDayDivider, isSameTimelineDay } from "@/channels/timeline";
import { useCallAnchorCardState } from "@/lib/call-thread-anchor";
import type { CallMedia } from "@/lib/calls/types";

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
  giphyApiKey: string;
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
  displayed: [messageId: string];
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

function onThreadScroll() {
  updateCurrentDayMarker();
  jumpToLive.updateDistance();
}

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

// Browser-tab refocus: re-pin the thread panel if the user was already at
// the edge before switching away. Mirrors the channel/DM message-list
// watchers; see `channels/messages.ts` / `dms/messages.ts`.
const { isWindowFocused } = useChatWindowVisibility();
watch(isWindowFocused, (focused, prev) => {
  if (focused && !prev && pinnedEdgeScroller.isPinnedAtEdge.value) {
    void scrollToPinnedEdge();
  }
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

// Thread "lobby" metadata for the rich header. Substitutes the bland
// "Thread > author" breadcrumb with: who started it, how many replies,
// who's been participating, and when activity last happened.
const threadReplyCount = computed(() => activeEntry.value?.count ?? 0);
const threadCallAnchorState = useCallAnchorCardState(
  () => activeEntry.value?.root ?? ({ body: "", author: "", threadId: null } as Pick<TimelineMessage, "body" | "author" | "callThread" | "threadId">),
  () => props.roomJid,
  () => threadReplyCount.value,
);

const threadRootAuthor = computed(() => activeEntry.value?.root?.author ?? null);

const threadPreview = computed(() => {
  const body = activeEntry.value?.root?.body?.trim() ?? "";
  if (!body) return "";
  return body.length > 96 ? `${body.slice(0, 95).trimEnd()}…` : body;
});

interface ThreadParticipant {
  nick: string;
  avatarUrl?: string | null;
  presence: OccupantPresence;
}

// Materialise the set of unique authors contributing to this thread —
// ordered by recency (most-recently-posted first), capped for the
// avatar stack. Exclude the current user so the stack answers
// "who else is in this thread?".
const threadParticipants = computed<ThreadParticipant[]>(() => {
  const entry = activeEntry.value;
  if (!entry) return [];
  const seen = new Set<string>();
  const ordered: ThreadParticipant[] = [];
  // Include every participant — the thread author themselves and the
  // current user — so the avatar stack is consistent regardless of
  // who replies. Walk newest → oldest to bias the visible avatars
  // toward recent participants.
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
  }
  const root = entry.root;
  if (root && !seen.has(root.author)) {
    ordered.push({
      nick: root.author,
      avatarUrl: props.avatarUrlByAuthor[root.author] ?? null,
      presence: props.roomPresence[root.author] ?? "offline",
    });
  }
  return ordered;
});

const MAX_THREAD_AVATARS = 4;
const visibleThreadParticipants = computed(() => threadParticipants.value.slice(0, MAX_THREAD_AVATARS));
const overflowParticipants = computed(() => Math.max(0, threadParticipants.value.length - visibleThreadParticipants.value.length));

const threadLastActivity = computed(() => {
  const entry = activeEntry.value;
  if (!entry) return null;
  const last = entry.directChildren.at(-1) ?? entry.root;
  return last?.createdAt ?? null;
});

function formatLastActivity(iso: string | null): string {
  if (!iso) return "";
  const ms = Date.now() - new Date(iso).getTime();
  if (Number.isNaN(ms) || ms < 0) return "";
  const seconds = Math.floor(ms / 1000);
  if (seconds < 45) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 2) return "1 min ago";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 2) return "1 hour ago";
  if (hours < 24) return `${hours} hours ago`;
  const days = Math.floor(hours / 24);
  if (days < 2) return "yesterday";
  return `${days} days ago`;
}

const threadLastActivityLabel = computed(() => formatLastActivity(threadLastActivity.value));

function hatsFor(author: string): OccupantHat[] {
  return props.roomHats[author] ?? [];
}

function authorityFor(author: string): OccupantAuthority | null {
  return props.roomAuthority[author] ?? null;
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

function onSend(
  body: string,
  markup: MarkupSpan[],
  references: MessageReference[],
  files?: Array<File | Blob>,
  linkPreview?: ComposerLinkPreviewSendPayload,
) {
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
  emit("send", body, markup, references, files, effectiveReply, override, linkPreview);
  replyingTo.value = null;
  draft.value = "";
}

// GIF picks mirror onSend's thread routing: derive the active thread (and
// parent, when nested) so the GIF message joins the open thread instead of
// landing in the parent channel timeline.
function onSelectGif(url: string) {
  const threadId = activeThreadId.value;
  if (!threadId) return;
  const override: { threadId: string; parentThreadId?: string } = { threadId };
  if (parentThreadId.value) override.parentThreadId = parentThreadId.value;
  emit("selectGif", url, override);
}

// Forward composer typing with the active thread context attached so the
// outbound XEP-0085 chat-state stanza can include the matching `<thread/>`.
function onTyping() {
  const threadId = activeThreadId.value;
  if (!threadId) {
    emit("typing");
    return;
  }
  const override: { threadId: string; parentThreadId?: string } = { threadId };
  if (parentThreadId.value) override.parentThreadId = parentThreadId.value;
  emit("typing", override);
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

function replyChildHasNestedThread(message: TimelineMessage): boolean {
  const entry = props.threadIndex.get(message.id);
  return !!entry && entry.count > 0;
}
</script>

<template>
  <div class="flex flex-col flex-1 min-h-0 bg-background border-l border-border">
    <!-- Thread lobby header — substantial metadata that helps you orient
         inside a thread. Top row: preview of the root message (or
         breadcrumb when nested). Bottom row: who started it · N replies ·
         when it last moved · who's been participating · close.
         When threadStack.length > 1 the top row becomes a breadcrumb
         trail so the user can pop back through nested sub-threads. -->
    <div class="chat-thread-header flex-shrink-0 glass-panel">
      <div class="chat-thread-header__row chat-thread-header__row--top">
        <template v-if="threadStack.length > 1">
          <div class="chat-thread-header__breadcrumb type-caption">
            <span class="chat-thread-header__breadcrumb-label">Threads</span>
            <template v-for="(label, i) in breadcrumbLabels" :key="i">
              <ChevronRight class="chat-thread-header__breadcrumb-sep" aria-hidden="true" />
              <button
                type="button"
                class="chat-thread-header__breadcrumb-crumb"
                :class="i === breadcrumbLabels.length - 1 ? 'chat-thread-header__breadcrumb-crumb--active' : ''"
                :title="label"
                @click="emit('popTo', i)"
              >{{ label }}</button>
            </template>
          </div>
        </template>
        <template v-else>
          <p
            v-if="threadPreview"
            class="chat-thread-header__preview"
            :title="threadPreview"
          >{{ threadPreview }}</p>
          <p v-else class="chat-thread-header__preview chat-thread-header__preview--empty">Thread</p>
        </template>
        <div class="chat-thread-header__actions">
          <button
            v-if="threadStack.length > 1"
            type="button"
            class="chat-icon-button hover:bg-muted lg:hidden"
            title="Go back"
            aria-label="Go back"
            @click="emit('popTo', threadStack.length - 2)"
          >
            <ChevronLeft class="w-4 h-4" />
          </button>
          <button
            type="button"
            class="chat-icon-button hover:bg-muted"
            title="Close thread"
            aria-label="Close thread"
            @click="emit('close')"
          >
            <X class="w-4 h-4" />
          </button>
        </div>
      </div>
      <div v-if="threadCallAnchorState" class="chat-thread-header__row">
        <CallAnchorCard
          :state="threadCallAnchorState"
          class="w-full"
          @join="joinThreadCallAnchor"
          @open-thread="onOpenThreadFromCard"
        />
      </div>
      <div class="chat-thread-header__row chat-thread-header__row--meta">
        <div class="chat-thread-header__meta type-caption">
          <span v-if="threadRootAuthor" class="chat-thread-header__meta-item">
            <span class="chat-thread-header__meta-label">Started by</span>
            <span class="chat-thread-header__meta-value">{{ threadRootAuthor }}</span>
          </span>
          <span class="chat-thread-header__meta-item">
            <span class="chat-thread-header__meta-value">
              <strong>{{ threadReplyCount }}</strong>
              {{ threadReplyCount === 1 ? "reply" : "replies" }}
            </span>
          </span>
          <span v-if="threadLastActivityLabel" class="chat-thread-header__meta-item">
            <span class="chat-thread-header__pulse" aria-hidden="true" />
            <span class="chat-thread-header__meta-value">active {{ threadLastActivityLabel }}</span>
          </span>
        </div>
        <div v-if="visibleThreadParticipants.length > 0" class="chat-thread-header__participants">
          <span class="chat-thread-header__avatars">
            <span
              v-for="participant in visibleThreadParticipants"
              :key="`thread-participant:${participant.nick}`"
              class="chat-thread-header__avatar-wrap"
              :title="`${participant.nick}`"
            >
              <AppAvatar
                :name="participant.nick"
                :src="participant.avatarUrl ?? null"
                :presence="participant.presence"
                size="xs"
              />
            </span>
          </span>
          <span v-if="overflowParticipants > 0" class="chat-thread-header__overflow">+{{ overflowParticipants }}</span>
        </div>
      </div>
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
            :call-room-jid="props.roomJid ?? null"
            :call-channel-id="props.channelId ?? null"
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
              :thread-reply-count="threadIndex.get(message.id)?.count ?? 0"
              :grouped="isGroupedFollowUp(message.id)"
              hide-thread-chip
              hide-reply-chip
              :reaction-mode-selected="reactionMode?.selectedMessageId === message.id"
              :invoke-extension-action="props.invokeExtensionAction"
              :link-preview-lookup="props.linkPreviewLookup"
              :link-preview-scope="props.linkPreviewScope"
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
