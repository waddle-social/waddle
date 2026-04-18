<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import { Search, X } from "lucide-vue-next";
import { searchEmoji } from "@/lib/emoji";

const props = withDefaults(
  defineProps<{
    open: boolean;
    /**
     * "popover" (default): absolutely positioned panel anchored to an anchor
     * element outside this component — desktop inline-toolbar usage.
     * "sheet": renders as a full-width in-flow grid that assumes its parent
     * controls positioning — used inside the mobile action sheet.
     */
    variant?: "popover" | "sheet";
  }>(),
  { variant: "popover" },
);

const emit = defineEmits<{
  select: [emoji: string];
  close: [];
}>();

const RECENT_KEY = "waddle:recent-emojis";
const RECENT_CAP = 24;

const COMMON_EMOJIS = [
  "👍", "❤️", "😂", "🎉", "👀", "🔥", "🙌", "💯",
  "😍", "🤔", "😊", "😭", "🙏", "✨", "👏", "💪",
  "🤣", "😎", "🥳", "😅", "🫡", "🫶", "🤝", "👋",
  "😀", "😁", "😆", "🥹", "😇", "🙃", "😉", "😌",
  "🤩", "🥰", "😘", "😋", "🤗", "🤭", "🤫", "🤐",
  "😐", "😑", "🙄", "😴", "🤤", "😪", "😮‍💨", "😤",
  "😡", "🤬", "🤯", "😳", "🥶", "🥵", "😨", "😰",
  "😥", "😓", "🤒", "🤕", "🤧", "🤮", "🤢", "😵",
  "🤠", "😈", "👻", "💀", "👽", "🤖", "💩", "🙈",
  "🙉", "🙊", "💖", "💔", "💕", "💞", "💓", "💗",
  "💘", "💝", "💟", "⚡", "🌟", "⭐", "☀️", "🌈",
  "🌸", "🌺", "🌻", "🌷", "🍀", "🎂", "🍰", "🍕",
  "🍔", "🍟", "🍜", "🍣", "🍺", "🍷", "🍾", "☕",
];

const query = ref("");
const recents = ref<string[]>([]);

function loadRecents() {
  if (typeof localStorage === "undefined") return;
  try {
    const raw = localStorage.getItem(RECENT_KEY);
    if (!raw) return;
    const parsed: unknown = JSON.parse(raw);
    if (Array.isArray(parsed)) {
      recents.value = parsed.filter((x): x is string => typeof x === "string").slice(0, RECENT_CAP);
    }
  } catch {
    // Corrupt storage — ignore.
  }
}

function saveRecent(emoji: string) {
  const next = [emoji, ...recents.value.filter((e) => e !== emoji)].slice(0, RECENT_CAP);
  recents.value = next;
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(next));
  } catch {
    // Quota or private mode — ignore.
  }
}

const searchResults = computed(() => {
  const q = query.value.trim();
  if (q.length < 2) return [];
  return searchEmoji(q, 60).map((r) => r.emoji);
});

const panelEl = ref<HTMLElement | null>(null);
const searchInputEl = ref<HTMLInputElement | null>(null);

function onSelect(emoji: string) {
  saveRecent(emoji);
  emit("select", emoji);
}

function onWindowPointer(event: PointerEvent | MouseEvent) {
  if (!panelEl.value) return;
  const target = event.target as Node | null;
  if (!target) return;
  if (!panelEl.value.contains(target)) emit("close");
}

function onKey(event: KeyboardEvent) {
  if (event.key === "Escape") emit("close");
}

function attachWindowListeners() {
  if (typeof window === "undefined") return;
  // Sheet variant is embedded inside a modal that already owns outside-tap
  // and Escape handling; attaching here would double-close on every click.
  if (props.variant === "sheet") return;
  window.addEventListener("pointerdown", onWindowPointer, true);
  window.addEventListener("keydown", onKey);
}

function detachWindowListeners() {
  if (typeof window === "undefined") return;
  window.removeEventListener("pointerdown", onWindowPointer, true);
  window.removeEventListener("keydown", onKey);
}

watch(
  () => props.open,
  (open) => {
    if (open) {
      loadRecents();
      attachWindowListeners();
      query.value = "";
      void nextTick().then(() => {
        searchInputEl.value?.focus();
      });
    } else {
      detachWindowListeners();
    }
  },
  { immediate: true },
);

onBeforeUnmount(detachWindowListeners);
</script>

<template>
  <div
    v-if="open"
    ref="panelEl"
    :role="variant === 'popover' ? 'dialog' : 'group'"
    aria-label="Choose a reaction"
    :class="[
      'flex flex-col overflow-hidden',
      variant === 'popover'
        ? 'absolute top-full right-0 mt-1 w-72 glass-panel border border-border rounded-xl z-50 shadow-2xl animate-fade-in max-h-80'
        : 'w-full max-h-[60vh]',
    ]"
    @pointerdown.stop
  >
    <div class="flex items-center gap-2.5 px-3 py-2 border-b border-border">
      <Search class="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" aria-hidden="true" />
      <input
        ref="searchInputEl"
        v-model="query"
        type="text"
        placeholder="Search emoji…"
        aria-label="Search emoji"
        class="flex-1 text-[13px] bg-transparent border-none focus:outline-none placeholder:text-muted-foreground/40"
      />
      <button
        type="button"
        class="p-1 rounded-md text-muted-foreground hover:text-foreground transition-colors flex-shrink-0"
        aria-label="Close emoji picker"
        @click="emit('close')"
      >
        <X class="w-3.5 h-3.5" aria-hidden="true" />
      </button>
    </div>

    <div class="flex-1 overflow-auto p-2">
      <template v-if="searchResults.length > 0">
        <div class="text-[10px] uppercase tracking-wider text-muted-foreground/70 px-1 pb-1">Results</div>
        <div :class="['grid gap-0.5', variant === 'sheet' ? 'grid-cols-7' : 'grid-cols-8']">
          <button
            v-for="e in searchResults"
            :key="`s-${e}`"
            type="button"
            :class="[
              'flex items-center justify-center leading-none rounded-md hover:bg-muted transition-all duration-150',
              variant === 'sheet' ? 'h-11 text-[24px]' : 'h-8 w-8 text-[18px]',
            ]"
            :aria-label="`React with ${e}`"
            @click="onSelect(e)"
          >{{ e }}</button>
        </div>
      </template>
      <template v-else-if="query.trim().length >= 2">
        <div class="text-center py-4 text-[12px] text-muted-foreground">No emoji found</div>
      </template>
      <template v-else>
        <template v-if="recents.length > 0">
          <div class="text-[10px] uppercase tracking-wider text-muted-foreground/70 px-1 pb-1">Recent</div>
          <div :class="['grid gap-0.5 mb-2', variant === 'sheet' ? 'grid-cols-7' : 'grid-cols-8']">
            <button
              v-for="e in recents"
              :key="`r-${e}`"
              type="button"
              :class="[
              'flex items-center justify-center leading-none rounded-md hover:bg-muted transition-all duration-150',
              variant === 'sheet' ? 'h-11 text-[24px]' : 'h-8 w-8 text-[18px]',
            ]"
              :aria-label="`React with ${e}`"
              @click="onSelect(e)"
            >{{ e }}</button>
          </div>
        </template>
        <div class="text-[10px] uppercase tracking-wider text-muted-foreground/70 px-1 pb-1">Common</div>
        <div :class="['grid gap-0.5', variant === 'sheet' ? 'grid-cols-7' : 'grid-cols-8']">
          <button
            v-for="e in COMMON_EMOJIS"
            :key="`c-${e}`"
            type="button"
            :class="[
              'flex items-center justify-center leading-none rounded-md hover:bg-muted transition-all duration-150',
              variant === 'sheet' ? 'h-11 text-[24px]' : 'h-8 w-8 text-[18px]',
            ]"
            :aria-label="`React with ${e}`"
            @click="onSelect(e)"
          >{{ e }}</button>
        </div>
      </template>
    </div>
  </div>
</template>
