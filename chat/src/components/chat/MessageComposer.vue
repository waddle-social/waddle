<script setup lang="ts">
import { ref, computed } from "vue";
import { Send, Image } from "lucide-vue-next";
import GifPicker from "@/components/chat/GifPicker.vue";
import { EMOJI_SHORTCODES } from "@/lib/emoji";

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
const showEmoji = ref(false);
const mentionQuery = ref("");
const emojiQuery = ref("");
const selectedIndex = ref(0);
const inputEl = ref<HTMLInputElement | null>(null);

const mentionResults = computed(() => {
  const q = mentionQuery.value.toLowerCase();
  if (!q) return props.memberNames.slice(0, 8);
  return props.memberNames.filter((n) => n.toLowerCase().includes(q)).slice(0, 8);
});

const emojiResults = computed(() => {
  const q = emojiQuery.value.toLowerCase();
  if (!q || q.length < 2) return [];
  const matches: { name: string; emoji: string }[] = [];
  for (const [name, emoji] of Object.entries(EMOJI_SHORTCODES)) {
    if (name.includes(q)) matches.push({ name, emoji });
    if (matches.length >= 8) break;
  }
  return matches;
});

// Unified autocomplete results for keyboard nav
const activeResults = computed(() => {
  if (showMentions.value) return mentionResults.value;
  if (showEmoji.value) return emojiResults.value;
  return [];
});

function onInput(e: Event) {
  const input = e.target as HTMLInputElement;
  draft.value = input.value;
  emit("typing");
  checkAutocomplete(input);
}

function checkAutocomplete(input: HTMLInputElement) {
  const pos = input.selectionStart ?? 0;
  const textBefore = input.value.slice(0, pos);

  // Check for @mention — unicode-aware: match @ followed by any non-whitespace chars
  const mentionMatch = textBefore.match(/(?:^|\s)@(\S*)$/);
  if (mentionMatch) {
    mentionQuery.value = mentionMatch[1];
    selectedIndex.value = 0;
    showMentions.value = true;
    showEmoji.value = false;
    return;
  }
  showMentions.value = false;

  // Check for :emoji shortcode
  const emojiMatch = textBefore.match(/(?:^|\s):([a-z0-9_+-]*)$/i);
  if (emojiMatch && emojiMatch[1].length >= 2) {
    emojiQuery.value = emojiMatch[1];
    selectedIndex.value = 0;
    showEmoji.value = true;
    return;
  }
  showEmoji.value = false;
}

function insertMention(username: string) {
  const input = inputEl.value;
  if (!input) return;

  const pos = input.selectionStart ?? 0;
  const textBefore = draft.value.slice(0, pos);
  const textAfter = draft.value.slice(pos);

  const replaced = textBefore.replace(/(?:^|\s)@\S*$/, (m) => {
    const prefix = m.match(/^\s/) ? m[0] : "";
    return `${prefix}@${username} `;
  });
  draft.value = replaced + textAfter;
  showMentions.value = false;

  const newPos = replaced.length;
  requestAnimationFrame(() => {
    input.focus();
    input.setSelectionRange(newPos, newPos);
  });
}

function insertEmoji(emoji: string) {
  const input = inputEl.value;
  if (!input) return;

  const pos = input.selectionStart ?? 0;
  const textBefore = draft.value.slice(0, pos);
  const textAfter = draft.value.slice(pos);

  const replaced = textBefore.replace(/(?:^|\s):[a-z0-9_+-]*$/i, (m) => {
    const prefix = m.match(/^\s/) ? m[0] : "";
    return `${prefix}${emoji} `;
  });
  draft.value = replaced + textAfter;
  showEmoji.value = false;

  const newPos = replaced.length;
  requestAnimationFrame(() => {
    input.focus();
    input.setSelectionRange(newPos, newPos);
  });
}

function onKeydown(e: KeyboardEvent) {
  if (activeResults.value.length > 0 && (showMentions.value || showEmoji.value)) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      selectedIndex.value = (selectedIndex.value + 1) % activeResults.value.length;
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      selectedIndex.value =
        (selectedIndex.value - 1 + activeResults.value.length) % activeResults.value.length;
      return;
    }
    if (e.key === "Tab" || e.key === "Enter") {
      e.preventDefault();
      if (showMentions.value) {
        insertMention(mentionResults.value[selectedIndex.value]);
      } else if (showEmoji.value) {
        insertEmoji(emojiResults.value[selectedIndex.value].emoji);
      }
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      showMentions.value = false;
      showEmoji.value = false;
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
        :class="i === selectedIndex ? 'bg-muted' : ''"
        @mousedown.prevent="insertMention(name)"
      >
        <span class="text-muted-foreground">@</span>{{ name }}
      </button>
    </div>

    <!-- :emoji autocomplete -->
    <div
      v-if="showEmoji && emojiResults.length > 0"
      class="absolute bottom-full left-6 mb-1 bg-background border border-foreground max-h-48 overflow-auto z-50 min-w-48"
    >
      <button
        v-for="(entry, i) in emojiResults"
        :key="entry.name"
        class="w-full px-3 py-1.5 text-left font-mono text-sm hover:bg-muted transition-colors flex items-center gap-2"
        :class="i === selectedIndex ? 'bg-muted' : ''"
        @mousedown.prevent="insertEmoji(entry.emoji)"
      >
        <span class="text-base">{{ entry.emoji }}</span>
        <span class="text-muted-foreground">:{{ entry.name }}:</span>
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
