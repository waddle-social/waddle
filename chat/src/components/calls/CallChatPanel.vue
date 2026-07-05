<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import MessageBody from "@/components/chat/MessageBody.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import type { MarkupSpan, MessageReference, TimelineMessage } from "@/lib/chat-ui";
import type { MentionCandidate } from "@/lib/mentions";
import type {
  ComposerLinkPreviewLookup,
  ComposerLinkPreviewSendPayload,
} from "@/lib/link-preview-composer";

/**
 * The in-call **Chat** panel: a scrollable list of the active call's
 * XEP-0201 thread messages plus the call-chat composer. Lives inside the
 * Dock's Chat tab. Purely presentational — the parent (ContentArea) owns
 * the thread, message data, and the send path; this panel renders them and
 * forwards composer intents.
 */
const props = defineProps<{
  messages: readonly TimelineMessage[];
  draft: string;
  /** Whether the Chat tab is the visible dock tab. Becoming visible re-pins to
   *  the latest message so the unread badge → newest-message flow lands right. */
  visible?: boolean;
  avatarUrlByAuthor: Record<string, string | null>;
  isSending?: boolean;
  disabled?: boolean;
  mentionCandidates?: MentionCandidate[];
  slowModeCooldown?: number;
  uploadProgress?: { uploading: boolean; progress: number; filename: string };
  inMuc?: boolean;
  linkPreviewLookup?: ComposerLinkPreviewLookup | null;
  linkPreviewScope?: string | null;
}>();

const emit = defineEmits<{
  "update:draft": [value: string];
  send: [
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files: Array<File | Blob> | undefined,
    linkPreview?: ComposerLinkPreviewSendPayload,
  ];
}>();

const hasMessages = computed(() => props.messages.length > 0);

function timeLabel(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

// Keep the latest message in view when one arrives — but only when the reader is
// already near the bottom, so scrolling up to read history isn't yanked away.
const messagesRef = ref<HTMLElement | null>(null);
const NEAR_BOTTOM_PX = 80;

function isNearBottom(el: HTMLElement): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight < NEAR_BOTTOM_PX;
}

function scrollToBottom(): void {
  const el = messagesRef.value;
  if (el) el.scrollTop = el.scrollHeight;
}

// The watcher runs before the DOM patch (default "pre" flush), so `isNearBottom`
// reflects the pre-append position; the actual scroll runs after the patch.
watch(
  () => props.messages.length,
  (next, prev) => {
    if (next <= prev) return;
    const el = messagesRef.value;
    if (!el || isNearBottom(el)) void nextTick(scrollToBottom);
  },
);

// Switching to the Chat tab is a `v-show` toggle (no remount), so neither
// `onMounted` nor the length watch fires. Re-pin to the newest message when the
// tab becomes visible so a reader following the unread badge sees it.
watch(
  () => props.visible,
  (visible) => {
    if (visible) void nextTick(scrollToBottom);
  },
);

onMounted(() => void nextTick(scrollToBottom));
</script>

<template>
  <div class="call-chat">
    <div ref="messagesRef" class="call-chat__messages" role="log" aria-label="Call chat messages">
      <p v-if="!hasMessages" class="call-chat__empty type-caption text-muted-foreground">
        No messages yet. Say hello.
      </p>
      <div
        v-for="m in messages"
        :key="m.id"
        class="call-chat__row"
        :data-message-id="m.id"
      >
        <AppAvatar
          :name="m.author"
          :src="avatarUrlByAuthor[m.author] ?? null"
          size="xs"
        />
        <div class="call-chat__bubble">
          <div class="call-chat__meta">
            <span class="call-chat__author type-control">{{ m.author }}</span>
            <time
              :datetime="m.createdAt"
              class="call-chat__time type-caption text-muted-foreground"
            >{{ timeLabel(m.createdAt) }}</time>
          </div>
          <MessageBody :message="m" compact />
        </div>
      </div>
    </div>
    <MessageComposer
      :draft="draft"
      channel-name="call chat"
      composer-label="Call chat composer"
      :is-forum-channel="false"
      :is-sending="!!isSending"
      :disabled="!!disabled"
      :mention-candidates="mentionCandidates ?? []"
      :slow-mode-cooldown="slowModeCooldown ?? 0"
      :upload-progress="uploadProgress ?? { uploading: false, progress: 0, filename: '' }"
      :in-muc="!!inMuc"
      :link-preview-lookup="linkPreviewLookup ?? null"
      :link-preview-scope="linkPreviewScope ?? null"
      :show-extensions="false"
      @update:draft="emit('update:draft', $event)"
      @send="(body, markup, references, files, linkPreview) => emit('send', body, markup, references, files, linkPreview)"
    />
  </div>
</template>

<style scoped>
.call-chat {
  flex: 1 1 auto;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.call-chat__messages {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  padding: 0.75rem;
}

.call-chat__empty {
  margin: auto;
  text-align: center;
}

.call-chat__row {
  display: flex;
  align-items: flex-start;
  gap: 0.5rem;
}

.call-chat__bubble {
  min-width: 0;
  flex: 1 1 auto;
}

.call-chat__meta {
  display: flex;
  align-items: baseline;
  gap: 0.5rem;
}

.call-chat__author {
  font-weight: 600;
}
</style>
