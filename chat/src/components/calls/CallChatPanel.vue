<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { css } from "styled-system/css";
import UserAvatar from "@/components/ui/UserAvatar.vue";
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
  /** Real bare JID behind a row's author, for the avatar; `null` = initials. */
  avatarJidFor: (message: TimelineMessage) => string | null;
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

// HuddlePanel board thread: avatar · author + time · body, composer below.
const rootClass = css({ flex: "1 1 auto", minHeight: 0, display: "flex", flexDirection: "column" });
const logClass = css({ flex: "1 1 auto", minHeight: 0, overflowY: "auto", display: "flex", flexDirection: "column", padding: "8px 0" });
const emptyClass = css({ margin: "auto", padding: "4", textAlign: "center", fontSize: "13px", color: "fg.muted" });
const rowClass = css({ display: "grid", gridTemplateColumns: "28px minmax(0, 1fr)", gap: "2px 10px", padding: "8px 16px", "& > :first-child": { gridRow: "1 / span 2" } });
const metaClass = css({ display: "flex", alignItems: "baseline", gap: "1.5", minWidth: 0 });
const authorClass = css({ font: "700 14px/1.3 {fonts.body}", color: "fg" });
const timeClass = css({ fontSize: "13px", color: "fg.muted" });
const bodyClass = css({ minWidth: 0, fontSize: "14px", lineHeight: "1.45" });
</script>

<template>
  <div :class="rootClass">
    <div ref="messagesRef" :class="logClass" role="log" aria-label="Call chat messages">
      <p v-if="!hasMessages" :class="emptyClass">
        No messages yet. Say hello.
      </p>
      <div
        v-for="m in messages"
        :key="m.id"
        :class="rowClass"
        :data-message-id="m.id"
      >
        <UserAvatar
          :name="m.author"
          :jid="avatarJidFor(m)"
          size="sm"
        />
        <div :class="metaClass">
          <span :class="authorClass">{{ m.author }}</span>
          <time
            :datetime="m.createdAt"
            :class="timeClass"
          >{{ timeLabel(m.createdAt) }}</time>
        </div>
        <div :class="bodyClass">
          <MessageBody :message="m" compact />
        </div>
      </div>
    </div>
    <MessageComposer
      :draft="draft"
      channel-name="call chat"
      :placeholder="inMuc ? 'Reply in the huddle thread' : 'Reply in the call thread'"
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
      @select-gif="(url) => emit('send', url, [], [], undefined)"
    />
  </div>
</template>
