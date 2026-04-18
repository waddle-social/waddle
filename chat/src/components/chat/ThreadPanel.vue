<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { X, CornerDownRight, MessageSquarePlus, ChevronRight } from "lucide-vue-next";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";
import type { TimelineMessage, MarkupSpan } from "@/lib/chat-ui";
import type { OccupantHat, OccupantPresence, RoomHats, RoomPresence } from "@/lib/xmpp-client";
import type { ThreadEntry, ThreadIndex } from "@/composables/useThreads";

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
  memberNames: string[];
  slowModeCooldown: number;
  isSending: boolean;
  uploadProgress: { uploading: boolean; progress: number; filename: string };
  channelName: string;
}>();

const emit = defineEmits<{
  close: [];
  popTo: [index: number];
  pushThread: [threadId: string];
  send: [
    body: string,
    markup: MarkupSpan[],
    files: Array<File | Blob> | undefined,
    replyTo: { id: string; author: string; body?: string } | undefined,
    threadOverride: { threadId: string; parentThreadId?: string },
  ];
  editMessage: [messageId: string, newBody: string, markup?: MarkupSpan[]];
  retractMessage: [messageId: string];
  reactMessage: [messageId: string, emoji: string];
  displayed: [messageId: string];
  selectGif: [url: string];
  typing: [];
}>();

const open = computed(() => props.threadStack.length > 0);
const activeThreadId = computed(() => props.threadStack[props.threadStack.length - 1] ?? null);
const parentThreadId = computed(() =>
  props.threadStack.length >= 2 ? props.threadStack[props.threadStack.length - 2] : undefined,
);
const activeEntry = computed(() =>
  activeThreadId.value ? props.resolveEntry(activeThreadId.value) ?? null : null,
);

const draft = ref("");

const replyingTo = ref<{ id: string; author: string; body?: string; preview?: string } | null>(null);

function beginReplyInThread(message: TimelineMessage) {
  replyingTo.value = {
    id: message.id,
    author: message.author,
    ...(message.body ? { body: message.body, preview: message.body } : {}),
  };
}

function cancelReplyInThread() {
  replyingTo.value = null;
}

// Close clears the draft and any pending reply target so opening a different
// thread starts fresh.
watch(activeThreadId, () => {
  replyingTo.value = null;
  draft.value = "";
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

function onSend(body: string, markup: MarkupSpan[], files?: Array<File | Blob>) {
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
  emit("send", body, markup, files, effectiveReply, override);
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
  <AppDrawer
    :open="open"
    side="right"
    width-class="w-[min(92vw,28rem)]"
    @update:open="(v: boolean) => { if (!v) emit('close') }"
  >
    <template #title>
      <div class="flex items-center gap-1 text-[13px] font-semibold truncate flex-wrap">
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
    </template>

    <div class="flex flex-col h-full">
      <div class="flex-1 min-h-0 overflow-auto px-3 py-3">
        <template v-if="activeEntry">
          <div v-if="!activeEntry.root" class="text-[12px] text-muted-foreground px-3 py-2 rounded-xl bg-muted/40 mb-2">
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
            @edit="(id, body, m) => emit('editMessage', id, body, m)"
            @retract="(id) => emit('retractMessage', id)"
            @react="(id, emoji) => emit('reactMessage', id, emoji)"
            @reply="beginReplyInThread"
            @open-thread="onOpenThreadFromCard"
          />

          <div
            v-for="child in activeEntry.directChildren"
            :key="child.id"
            class="relative group/thread-child"
          >
            <MessageCard
              :message="child"
              :current-user="currentUser"
              :avatar-url="avatarUrlByAuthor[child.author] ?? null"
              :hats="hatsFor(child.author)"
              :presence="presenceFor(child.author)"
              :last-seen="roomLastSeen[child.author]"
              :author-jid="authorJidByNick?.[child.author]"
              :thread-reply-count="threadIndex.get(child.id)?.count ?? 0"
              hide-thread-chip
              @edit="(id, body, m) => emit('editMessage', id, body, m)"
              @retract="(id) => emit('retractMessage', id)"
              @react="(id, emoji) => emit('reactMessage', id, emoji)"
              @reply="beginReplyInThread"
              @open-thread="onOpenThreadFromCard"
            />
            <div class="flex flex-wrap items-center gap-2 pl-12 pb-1.5 -mt-0.5">
              <button
                v-if="replyChildHasNestedThread(child)"
                type="button"
                class="inline-flex items-center gap-1 text-[11px] text-primary/80 hover:text-primary transition-colors"
                @click="onOpenThreadFromCard(child.id)"
              >
                <CornerDownRight class="w-3 h-3" />
                <span>{{ threadIndex.get(child.id)?.count ?? 0 }} in sub-thread</span>
              </button>
              <button
                v-else
                type="button"
                class="inline-flex items-center gap-1 text-[11px] text-muted-foreground hover:text-primary transition-colors opacity-60 group-hover/thread-child:opacity-100 focus-visible:opacity-100"
                title="Start sub-thread"
                @click="startSubThread(child)"
              >
                <MessageSquarePlus class="w-3 h-3" />
                <span>Start sub-thread</span>
              </button>
            </div>
          </div>

          <div
            v-if="activeEntry.directChildren.length === 0"
            class="text-center py-8 text-[12px] text-muted-foreground"
          >
            No replies yet. Start the conversation.
          </div>
        </template>
        <div
          v-else
          class="text-center py-10 text-[13px] text-muted-foreground"
        >
          Loading thread...
        </div>
      </div>

      <div class="border-t border-border flex-shrink-0">
        <MessageComposer
          v-model:draft="draft"
          :channel-name="`thread in ${channelName}`"
          :is-sending="isSending"
          :disabled="false"
          :tenor-api-key="tenorApiKey"
          :member-names="memberNames"
          :slow-mode-cooldown="slowModeCooldown"
          :upload-progress="uploadProgress"
          :replying-to="replyingTo"
          @send="onSend"
          @cancel-reply="cancelReplyInThread"
          @typing="emit('typing')"
          @select-gif="(url: string) => emit('selectGif', url)"
        />
      </div>
    </div>

    <div class="absolute top-2 right-12 flex items-center gap-1 pointer-events-none">
      <!-- Kept for a11y parity; AppDrawer already has a close button. -->
      <span class="sr-only">
        <button type="button" @click="emit('close')"><X class="w-4 h-4" /></button>
      </span>
    </div>
  </AppDrawer>
</template>
