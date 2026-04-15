<script setup lang="ts">
import { ref, computed } from "vue";
import { Send, Image } from "lucide-vue-next";
import GifPicker from "@/components/chat/GifPicker.vue";
import { searchEmoji } from "@/lib/emoji";

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

function onInput(e: Event) {
  const input = e.target as HTMLInputElement;
  draft.value = input.value;
  emit("typing");
  checkAutocomplete(input);
}

function checkAutocomplete(input: HTMLInputElement) {
  const pos = input.selectionStart ?? 0;
  const textBefore = input.value.slice(0, pos);

  const mentionMatch = textBefore.match(/(?:^|\s)@(\S*)$/);
  if (mentionMatch) {
    mentionQuery.value = mentionMatch[1];
    selectedIndex.value = 0;
    showMentions.value = true;
    showEmoji.value = false;
    return;
  }
  showMentions.value = false;

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
  <div class="relative px-4 py-3 flex items-center gap-2.5 flex-shrink-0">
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
    <input
      ref="inputEl"
      :value="draft"
      :placeholder="slowModeCooldown > 0 ? `Slow mode — wait ${slowModeCooldown}s` : `Message #${channelName}`"
      :disabled="disabled || slowModeCooldown > 0"
      class="flex-1 rounded-xl focus:outline-none px-4 bg-muted text-[13px] h-10 placeholder:text-muted-foreground/40 transition-all duration-300 focus:ring-2 focus:ring-primary/20 focus:shadow-[0_0_16px_var(--glow)]"
      :class="slowModeCooldown > 0 ? 'opacity-40' : ''"
      @input="onInput"
      @keydown="onKeydown"
    />
    <button
      class="h-10 w-10 flex items-center justify-center bg-primary text-primary-foreground rounded-xl hover:shadow-[0_0_20px_var(--glow-strong)] transition-all duration-300 disabled:opacity-20 flex-shrink-0"
      :disabled="isSending || disabled || !draft.trim() || slowModeCooldown > 0"
      @click="emit('send')"
    >
      <span v-if="slowModeCooldown > 0" class="text-[10px] font-bold font-mono tabular-nums">{{ slowModeCooldown }}</span>
      <Send v-else class="w-4 h-4" />
    </button>
  </div>
</template>
