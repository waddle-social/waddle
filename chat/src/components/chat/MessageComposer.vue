<script setup lang="ts">
import { ref, computed } from "vue";
import { Send, Image } from "lucide-vue-next";
import GifPicker from "@/components/chat/GifPicker.vue";

const draft = defineModel<string>("draft", { required: true });

const props = defineProps<{
  channelName: string;
  isSending: boolean;
  disabled: boolean;
  tenorApiKey: string;
  memberNames: string[];
}>();

const emit = defineEmits<{
  send: [];
  typing: [];
  selectGif: [url: string];
}>();

const showGifPicker = ref(false);
const showMentions = ref(false);
const mentionQuery = ref("");
const selectedMentionIndex = ref(0);
const inputEl = ref<HTMLInputElement | null>(null);

const mentionResults = computed(() => {
  const q = mentionQuery.value.toLowerCase();
  if (!q) return props.memberNames.slice(0, 8);
  return props.memberNames.filter((n) => n.toLowerCase().includes(q)).slice(0, 8);
});

function onInput(e: Event) {
  const input = e.target as HTMLInputElement;
  draft.value = input.value;
  emit("typing");
  checkMentionTrigger(input);
}

function checkMentionTrigger(input: HTMLInputElement) {
  const pos = input.selectionStart ?? 0;
  const textBefore = input.value.slice(0, pos);
  // Find the last @ that starts a mention (preceded by start-of-string or whitespace)
  const match = textBefore.match(/(?:^|\s)@(\w*)$/);
  if (match) {
    mentionQuery.value = match[1];
    selectedMentionIndex.value = 0;
    showMentions.value = true;
  } else {
    showMentions.value = false;
  }
}

function insertMention(username: string) {
  const input = inputEl.value;
  if (!input) return;

  const pos = input.selectionStart ?? 0;
  const textBefore = draft.value.slice(0, pos);
  const textAfter = draft.value.slice(pos);

  // Replace the @query with @username
  const replaced = textBefore.replace(/(?:^|\s)@\w*$/, (m) => {
    const prefix = m.startsWith(" ") ? " " : "";
    return `${prefix}@${username} `;
  });
  draft.value = replaced + textAfter;
  showMentions.value = false;

  // Restore cursor position after Vue updates the input
  const newPos = replaced.length;
  requestAnimationFrame(() => {
    input.focus();
    input.setSelectionRange(newPos, newPos);
  });
}

function onKeydown(e: KeyboardEvent) {
  if (showMentions.value && mentionResults.value.length > 0) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      selectedMentionIndex.value = (selectedMentionIndex.value + 1) % mentionResults.value.length;
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      selectedMentionIndex.value =
        (selectedMentionIndex.value - 1 + mentionResults.value.length) % mentionResults.value.length;
      return;
    }
    if (e.key === "Tab" || e.key === "Enter") {
      e.preventDefault();
      insertMention(mentionResults.value[selectedMentionIndex.value]);
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      showMentions.value = false;
      return;
    }
  }

  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    emit("send");
  }
}

function onGifSelected(url: string) {
  showGifPicker.value = false;
  emit("selectGif", url);
}
</script>

<template>
  <div class="relative h-16 border-t border-foreground bg-background px-6 flex items-center gap-3 flex-shrink-0">
    <GifPicker
      v-if="showGifPicker"
      :api-key="tenorApiKey"
      @select="onGifSelected"
      @close="showGifPicker = false"
    />

    <!-- @mention autocomplete -->
    <div
      v-if="showMentions && mentionResults.length > 0"
      class="absolute bottom-full left-6 mb-1 bg-background border border-foreground max-h-48 overflow-auto z-50 min-w-48"
    >
      <button
        v-for="(name, i) in mentionResults"
        :key="name"
        class="w-full px-3 py-1.5 text-left font-mono text-sm hover:bg-muted transition-colors flex items-center gap-2"
        :class="i === selectedMentionIndex ? 'bg-muted' : ''"
        @mousedown.prevent="insertMention(name)"
      >
        <span class="text-muted-foreground">@</span>{{ name }}
      </button>
    </div>

    <button
      class="h-9 w-9 flex items-center justify-center hover:bg-muted transition-colors flex-shrink-0"
      :class="showGifPicker ? 'bg-muted' : ''"
      title="GIF"
      :disabled="disabled"
      @click="showGifPicker = !showGifPicker"
    >
      <Image class="w-3.5 h-3.5" />
    </button>
    <input
      ref="inputEl"
      :value="draft"
      :placeholder="`Message #${channelName}`"
      :disabled="disabled"
      class="flex-1 font-mono border border-foreground focus:outline-none focus:ring-2 focus:ring-foreground px-3 bg-background text-sm h-9"
      @input="onInput"
      @keydown="onKeydown"
    />
    <button
      class="h-9 w-9 flex items-center justify-center bg-foreground text-background hover:bg-foreground/90 transition-colors disabled:opacity-50 flex-shrink-0"
      :disabled="isSending || disabled || !draft.trim()"
      @click="emit('send')"
    >
      <Send class="w-3.5 h-3.5" />
    </button>
  </div>
</template>
