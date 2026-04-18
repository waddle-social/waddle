<script setup lang="ts">
import { ref, computed, watch, onBeforeUnmount } from "vue";
import { Send, Image, X } from "lucide-vue-next";
import GifPicker from "@/components/chat/GifPicker.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import { searchEmoji } from "@/lib/emoji";
import { serializeTiptapToXep0393 } from "@/lib/editor/xep0393-serializer";
import { getComposerEscapeAction } from "@/lib/reply-ux";
import { extractImagesFromEvent } from "@/lib/xmpp/file-upload";
import type { MarkupSpan } from "@/lib/chat-ui";

interface PendingAttachment {
  id: string;
  file: File | Blob;
  previewUrl: string;
  name: string;
}

const draft = defineModel<string>("draft", { required: true });

const props = defineProps<{
  channelName: string;
  isSending: boolean;
  disabled: boolean;
  tenorApiKey: string;
  memberNames: string[];
  slowModeCooldown: number;
  uploadProgress: { uploading: boolean; progress: number; filename: string };
  replyingTo?: { id: string; author: string; preview?: string } | null;
  /** True in channel mode, where a reply auto-joins its parent's thread. */
  replyJoinsThread?: boolean;
}>();

const emit = defineEmits<{
  send: [body: string, markup: MarkupSpan[], files?: Array<File | Blob>];
  typing: [];
  selectGif: [url: string];
  cancelReply: [];
}>();

const replyAuthorName = computed(() => {
  const author = props.replyingTo?.author;
  if (!author) return "";
  return author.includes("/") ? author.split("/").pop()! : author.split("@")[0] ?? author;
});

const showGifPicker = ref(false);
const showMentions = ref(false);
const showEmoji = ref(false);
const mentionQuery = ref("");
const emojiQuery = ref("");
const selectedIndex = ref(0);
const editorRef = ref<InstanceType<typeof ChatEditor> | null>(null);
const setEditorRef = (instance: InstanceType<typeof ChatEditor> | null) => {
  editorRef.value = instance;
};

const pendingAttachments = ref<PendingAttachment[]>([]);

function addAttachments(files: Array<File | Blob>) {
  const next = pendingAttachments.value.slice();
  for (const file of files) {
    const name = file instanceof File ? file.name : `image-${Date.now()}.png`;
    next.push({
      id: crypto.randomUUID(),
      file,
      name,
      previewUrl: URL.createObjectURL(file),
    });
  }
  pendingAttachments.value = next;
}

function removeAttachment(id: string) {
  const found = pendingAttachments.value.find((a) => a.id === id);
  if (found) URL.revokeObjectURL(found.previewUrl);
  pendingAttachments.value = pendingAttachments.value.filter((a) => a.id !== id);
}

onBeforeUnmount(() => {
  for (const a of pendingAttachments.value) URL.revokeObjectURL(a.previewUrl);
});

/** Get the underlying TipTap Editor instance from the ChatEditor ref. */
function getTiptapEditor() {
  const e = editorRef.value as any;
  return e?.editor?.value ?? e?.editor ?? null;
}

// Track the ProseMirror position range for the active autocomplete trigger
const triggerRange = ref<{ from: number; to: number } | null>(null);

function stripDiacritics(s: string): string {
  return s.normalize("NFD").replace(/[\u0300-\u036f]/g, "").toLowerCase();
}

const BROADCAST_MENTIONS = ["everyone", "here"];

const mentionResults = computed(() => {
  const q = mentionQuery.value.toLowerCase();
  const qNorm = stripDiacritics(mentionQuery.value);
  const allNames = [...BROADCAST_MENTIONS, ...props.memberNames];
  if (!q) return allNames.slice(0, 8);
  return allNames.filter((n) => {
    const lower = n.toLowerCase();
    return lower.includes(q) || stripDiacritics(n).includes(qNorm);
  }).slice(0, 8);
});

const emojiResults = computed(() => searchEmoji(emojiQuery.value));

const activeResults = computed(() => {
  if (showMentions.value) return mentionResults.value;
  if (showEmoji.value) return emojiResults.value;
  return [];
});

/** Whether the composer has nothing sendable (no text and no pending attachments). */
const isEmpty = computed(() => !draft.value.trim() && pendingAttachments.value.length === 0);

function onEditorUpdate(doc: Record<string, unknown>) {
  // Keep draft in sync as a plain text representation
  const serialized = serializeTiptapToXep0393(doc as any);
  draft.value = serialized.body;
  emit("typing");
  checkAutocompleteFromEditor();
}

function checkAutocompleteFromEditor() {
  const editor = editorRef.value;
  if (!editor) return;
  const json = editor.getJSON?.();
  if (!json) return;

  // Access the underlying TipTap editor to get ProseMirror state
  const tiptapEditor = getTiptapEditor();
  if (!tiptapEditor?.state) return;

  const { selection, doc } = tiptapEditor.state;
  const pos = selection.from;
  const textBefore = doc.textBetween(0, pos, "\n", "\uFFFC");

  const mentionMatch = textBefore.match(/(?:^|\s)@(\S*)$/);
  if (mentionMatch) {
    mentionQuery.value = mentionMatch[1];
    selectedIndex.value = 0;
    showMentions.value = true;
    showEmoji.value = false;
    // Calculate PM range for the trigger
    const triggerLen = mentionMatch[0].length;
    const triggerStart = textBefore.length - triggerLen;
    // Map text offset back to PM position: find the PM pos for the trigger start
    triggerRange.value = {
      from: pos - mentionMatch[0].trimStart().length,
      to: pos,
    };
    return;
  }
  showMentions.value = false;

  const emojiMatch = textBefore.match(/(?:^|\s):([a-z0-9_+-]*)$/i);
  if (emojiMatch && emojiMatch[1].length >= 2) {
    emojiQuery.value = emojiMatch[1];
    selectedIndex.value = 0;
    showEmoji.value = true;
    triggerRange.value = {
      from: pos - emojiMatch[0].trimStart().length,
      to: pos,
    };
    return;
  }
  showEmoji.value = false;
}

function insertMention(username: string) {
  const tiptapEditor = getTiptapEditor();
  if (!tiptapEditor || !triggerRange.value) return;

  const replacement = `@${username} `;
  tiptapEditor.chain()
    .focus()
    .insertContentAt(triggerRange.value, replacement)
    .run();

  showMentions.value = false;
  triggerRange.value = null;
}

function insertEmoji(emoji: string) {
  const tiptapEditor = getTiptapEditor();
  if (!tiptapEditor || !triggerRange.value) return;

  const replacement = `${emoji} `;
  tiptapEditor.chain()
    .focus()
    .insertContentAt(triggerRange.value, replacement)
    .run();

  showEmoji.value = false;
  triggerRange.value = null;
}

function onSend(doc: Record<string, unknown>) {
  if (showMentions.value || showEmoji.value) {
    // If autocomplete is open, Enter selects instead of sending
    if (showMentions.value && mentionResults.value.length > 0) {
      insertMention(mentionResults.value[selectedIndex.value]);
    } else if (showEmoji.value && emojiResults.value.length > 0) {
      insertEmoji(emojiResults.value[selectedIndex.value].emoji);
    }
    return;
  }

  const serialized = serializeTiptapToXep0393(doc as any);
  const text = serialized.body.trim();
  const attachments = pendingAttachments.value;

  if (!text && attachments.length === 0) return;

  // Detach attachments before emitting so re-entry (e.g. Enter burst) cannot
  // double-send them. Revoke preview URLs once ownership transfers to the parent.
  const files = attachments.map((a) => a.file);
  if (attachments.length > 0) {
    pendingAttachments.value = [];
    for (const a of attachments) URL.revokeObjectURL(a.previewUrl);
  }

  emit("send", serialized.body, serialized.markup, files.length > 0 ? files : undefined);
}

defineExpose({ addAttachments });

function onEditorCancel() {
  const action = getComposerEscapeAction({
    showMentions: showMentions.value,
    showEmoji: showEmoji.value,
    isReplyingTo: !!props.replyingTo,
  });

  if (action === "dismiss-autocomplete") {
    showMentions.value = false;
    showEmoji.value = false;
    triggerRange.value = null;
    return;
  }

  if (action === "cancel-reply") {
    emit("cancelReply");
  }
}

function onKeydown(e: KeyboardEvent) {
  if (activeResults.value.length > 0 && (showMentions.value || showEmoji.value)) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      e.stopPropagation();
      selectedIndex.value = (selectedIndex.value + 1) % activeResults.value.length;
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      e.stopPropagation();
      selectedIndex.value =
        (selectedIndex.value - 1 + activeResults.value.length) % activeResults.value.length;
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      e.stopPropagation();
      if (showMentions.value) {
        insertMention(mentionResults.value[selectedIndex.value]);
      } else if (showEmoji.value) {
        insertEmoji(emojiResults.value[selectedIndex.value].emoji);
      }
      return;
    }
  }
}

function onGifSelected(url: string) {
  showGifPicker.value = false;
  emit("selectGif", url);
}

function onPaste(e: ClipboardEvent) {
  const files = extractImagesFromEvent(e);
  if (files.length > 0) {
    e.preventDefault();
    addAttachments(files);
  }
}

// Clear editor content when draft is reset externally (e.g. after successful send)
watch(
  () => draft.value,
  (newVal) => {
    if (newVal === "" && editorRef.value && !editorRef.value.isEmpty()) {
      editorRef.value.clear();
    }
  },
);
</script>

<template>
  <div class="relative px-4 py-3 flex-shrink-0" @keydown="onKeydown" @paste="onPaste">
    <!-- Reply context chip. In channel mode every reply joins a thread, so we
         tell the user their message will show up in the thread panel. -->
    <div
      v-if="replyingTo"
      class="flex flex-wrap items-center gap-x-2 gap-y-1 px-3 py-1.5 mb-2 rounded-xl bg-muted/70 border border-border text-[12px] animate-fade-in"
    >
      <span class="text-muted-foreground">Replying to</span>
      <span class="font-medium text-primary/90">@{{ replyAuthorName }}</span>
      <span v-if="replyingTo.preview" class="text-muted-foreground truncate flex-1 min-w-0">{{ replyingTo.preview }}</span>
      <span v-if="replyJoinsThread" class="text-[11px] text-primary/70 ml-auto">in thread</span>
      <button
        type="button"
        class="h-5 w-5 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-colors"
        :class="{ 'ml-auto': !replyJoinsThread }"
        title="Cancel reply"
        aria-label="Cancel reply"
        @click="emit('cancelReply')"
      >
        <X class="w-3 h-3" />
      </button>
    </div>

    <!-- Pending attachment previews -->
    <div
      v-if="pendingAttachments.length > 0"
      class="flex flex-wrap gap-2 mb-2 animate-fade-in"
    >
      <div
        v-for="att in pendingAttachments"
        :key="att.id"
        class="relative group/att rounded-xl border border-border bg-muted overflow-hidden"
      >
        <img
          :src="att.previewUrl"
          :alt="att.name"
          class="h-20 w-20 object-cover"
        />
        <button
          type="button"
          class="absolute top-1 right-1 h-5 w-5 flex items-center justify-center rounded-full bg-background/90 text-muted-foreground hover:text-destructive border border-border shadow-sm opacity-0 group-hover/att:opacity-100 focus:opacity-100 transition-opacity"
          :title="`Remove ${att.name}`"
          :aria-label="`Remove attachment ${att.name}`"
          @click="removeAttachment(att.id)"
        >
          <X class="w-3 h-3" aria-hidden="true" />
        </button>
      </div>
    </div>

    <!-- Upload progress bar -->
    <div
      v-if="uploadProgress.uploading"
      class="flex items-center gap-2 text-[11px] text-muted-foreground mb-2 animate-fade-in"
    >
      <span class="truncate max-w-40">Uploading {{ uploadProgress.filename }}...</span>
      <div class="flex-1 h-1 bg-muted rounded-full overflow-hidden">
        <div
          class="h-full bg-primary rounded-full transition-all duration-300"
          :style="{ width: `${Math.round(uploadProgress.progress * 100)}%` }"
        />
      </div>
      <span class="tabular-nums font-mono">{{ Math.round(uploadProgress.progress * 100) }}%</span>
    </div>

    <div class="flex items-center gap-2.5">
    <GifPicker
      v-if="showGifPicker"
      :api-key="tenorApiKey"
      @select="onGifSelected"
      @close="showGifPicker = false"
    />

    <!-- @mention autocomplete -->
    <div
      v-if="showMentions && mentionResults.length > 0"
      class="absolute bottom-full left-4 mb-2 glass-panel border border-border rounded-xl max-h-48 overflow-auto z-50 min-w-48 shadow-xl animate-fade-in"
    >
      <div class="py-1">
        <button
          v-for="(name, i) in mentionResults"
          :key="name"
          class="w-full px-3 py-2 text-left text-[13px] hover:bg-muted transition-colors flex items-center gap-2 rounded-lg"
          :class="i === selectedIndex ? 'bg-muted' : ''"
          @mousedown.prevent="insertMention(name)"
        >
          <span class="text-primary text-[11px]">@</span>
          <span class="font-medium">{{ name }}</span>
        </button>
      </div>
    </div>

    <!-- :emoji autocomplete -->
    <div
      v-if="showEmoji && emojiResults.length > 0"
      class="absolute bottom-full left-4 mb-2 glass-panel border border-border rounded-xl max-h-48 overflow-auto z-50 min-w-48 shadow-xl animate-fade-in"
    >
      <div class="py-1">
        <button
          v-for="(entry, i) in emojiResults"
          :key="entry.name"
          class="w-full px-3 py-2 text-left text-[13px] hover:bg-muted transition-colors flex items-center gap-2 rounded-lg"
          :class="i === selectedIndex ? 'bg-muted' : ''"
          @mousedown.prevent="insertEmoji(entry.emoji)"
        >
          <span>{{ entry.emoji }}</span>
          <span class="text-muted-foreground text-[11px]">:{{ entry.name }}:</span>
        </button>
      </div>
    </div>

    <button
      class="h-10 w-10 flex items-center justify-center rounded-xl transition-all duration-200 flex-shrink-0"
      :class="showGifPicker ? 'bg-muted text-primary' : 'text-muted-foreground hover:bg-muted hover:text-primary'"
      title="Open GIF picker"
      aria-label="Open GIF picker"
      :disabled="disabled"
      @click="showGifPicker = !showGifPicker"
    >
      <Image class="w-4 h-4" aria-hidden="true" />
    </button>
    <ChatEditor
      :ref="setEditorRef"
      :placeholder="slowModeCooldown > 0 ? `Slow mode — wait ${slowModeCooldown}s` : `Message #${channelName}`"
      :disabled="disabled || slowModeCooldown > 0"
      @send="onSend"
      @update="onEditorUpdate"
      @cancel="onEditorCancel"
    />
    <button
      class="h-10 w-10 flex items-center justify-center bg-primary text-primary-foreground rounded-xl hover:shadow-[0_0_20px_var(--glow-strong)] transition-all duration-300 disabled:opacity-20 flex-shrink-0"
      :disabled="isSending || disabled || isEmpty || slowModeCooldown > 0"
      aria-label="Send message"
      @click="editorRef?.getJSON?.() && onSend(editorRef.getJSON()!)"
    >
      <span v-if="slowModeCooldown > 0" class="text-[10px] font-bold font-mono tabular-nums">{{ slowModeCooldown }}</span>
      <Send v-else class="w-4 h-4" aria-hidden="true" />
    </button>
    </div>
  </div>
</template>
