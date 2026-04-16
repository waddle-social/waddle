<script setup lang="ts">
import { ref, computed, watch } from "vue";
import { Send, Image, Link2, Link2Off } from "lucide-vue-next";
import GifPicker from "@/components/chat/GifPicker.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import { searchEmoji } from "@/lib/emoji";
import { serializeTiptapToXep0393 } from "@/lib/editor/xep0393-serializer";
import type { MarkupSpan } from "@/lib/chat-ui";

const draft = defineModel<string>("draft", { required: true });

const props = defineProps<{
  channelName: string;
  isSending: boolean;
  disabled: boolean;
  tenorApiKey: string;
  memberNames: string[];
  slowModeCooldown: number;
}>();

const emit = defineEmits<{
  send: [body: string, markup: MarkupSpan[], suppressPreview?: boolean];
  typing: [];
  selectGif: [url: string];
}>();

/**
 * Per-message "Preview link" toggle. Default on; flipping it off sends
 * a `<no-preview xmlns='urn:waddle:link-preview:0'/>` hint that tells
 * the server's link-preview enricher to skip this message. Resets back
 * to true after each successful send so the next message starts fresh.
 */
const previewsEnabled = ref(true);

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

/** Whether the editor content is empty (for send button disabled state). */
const isEmpty = computed(() => !draft.value.trim());

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
  if (!serialized.body.trim()) return;

  emit("send", serialized.body, serialized.markup, !previewsEnabled.value || undefined);

  // Reset the toggle for the next message.
  previewsEnabled.value = true;
}

function onEditorCancel() {
  if (showMentions.value || showEmoji.value) {
    showMentions.value = false;
    showEmoji.value = false;
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
  <div class="relative px-4 py-3 flex items-center gap-2.5 flex-shrink-0" @keydown="onKeydown">
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
      class="h-9 w-9 flex items-center justify-center rounded-xl transition-all duration-200 flex-shrink-0"
      :class="showGifPicker ? 'bg-muted text-primary' : 'text-muted-foreground hover:bg-muted hover:text-primary'"
      title="GIF"
      :disabled="disabled"
      @click="showGifPicker = !showGifPicker"
    >
      <Image class="w-4 h-4" />
    </button>
    <button
      class="h-9 w-9 flex items-center justify-center rounded-xl transition-all duration-200 flex-shrink-0"
      :class="previewsEnabled ? 'text-muted-foreground hover:bg-muted hover:text-primary' : 'bg-muted text-muted-foreground'"
      :title="previewsEnabled ? 'Link previews: on — click to skip preview for next message' : 'Link previews: off for next message — click to re-enable'"
      :disabled="disabled"
      @click="previewsEnabled = !previewsEnabled"
    >
      <Link2 v-if="previewsEnabled" class="w-4 h-4" />
      <Link2Off v-else class="w-4 h-4" />
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
      @click="editorRef?.getJSON?.() && onSend(editorRef.getJSON()!)"
    >
      <span v-if="slowModeCooldown > 0" class="text-[10px] font-bold font-mono tabular-nums">{{ slowModeCooldown }}</span>
      <Send v-else class="w-4 h-4" />
    </button>
  </div>
</template>
