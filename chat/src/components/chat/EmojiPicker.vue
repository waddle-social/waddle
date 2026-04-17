<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { Search, X } from "lucide-vue-next";
import { searchEmoji } from "@/lib/emoji";
import { COMMON_REACTIONS, loadRecents, pushRecent } from "@/lib/emoji-picker-data";

const props = defineProps<{
  open: boolean;
  anchor: HTMLElement | null;
}>();

const emit = defineEmits<{
  select: [emoji: string];
  close: [];
}>();

const panelEl = ref<HTMLElement | null>(null);
const searchInputEl = ref<HTMLInputElement | null>(null);
const query = ref("");
const recents = ref<string[]>([]);
const position = ref<{ top: number; left: number }>({ top: 0, left: 0 });

const PANEL_WIDTH = 288;
const PANEL_MAX_HEIGHT = 320;
const GAP = 8;
const VIEWPORT_MARGIN = 8;

const searchResults = computed(() => {
  const q = query.value.trim();
  if (q.length < 2) return [];
  return searchEmoji(q, 64).map((r) => r.emoji);
});

const hasQuery = computed(() => query.value.trim().length >= 2);

const gridEmojis = computed<readonly string[]>(() =>
  hasQuery.value ? searchResults.value : COMMON_REACTIONS,
);

const firstVisibleEmoji = computed(() => {
  if (hasQuery.value) return searchResults.value[0] ?? null;
  return recents.value[0] ?? COMMON_REACTIONS[0] ?? null;
});

function computePosition() {
  const anchor = props.anchor;
  if (!anchor) return;
  const rect = anchor.getBoundingClientRect();
  const vw = window.innerWidth;
  const vh = window.innerHeight;

  const spaceBelow = vh - rect.bottom;
  const spaceAbove = rect.top;
  const placeAbove = spaceBelow < PANEL_MAX_HEIGHT + GAP && spaceAbove > spaceBelow;

  let top = placeAbove
    ? Math.max(VIEWPORT_MARGIN, rect.top - GAP - PANEL_MAX_HEIGHT)
    : rect.bottom + GAP;

  let left = rect.right - PANEL_WIDTH;
  if (left < VIEWPORT_MARGIN) left = VIEWPORT_MARGIN;
  if (left + PANEL_WIDTH > vw - VIEWPORT_MARGIN) left = vw - VIEWPORT_MARGIN - PANEL_WIDTH;

  position.value = { top, left };
}

function onWindowClick(event: MouseEvent) {
  if (!props.open) return;
  const target = event.target as Node | null;
  if (!panelEl.value || !target) return;
  if (panelEl.value.contains(target)) return;
  if (props.anchor && props.anchor.contains(target)) return;
  emit("close");
}

function onKey(event: KeyboardEvent) {
  if (!props.open) return;
  if (event.key === "Escape") {
    event.stopPropagation();
    emit("close");
    return;
  }
  if (event.key === "Enter" && document.activeElement === searchInputEl.value) {
    event.preventDefault();
    const pick = firstVisibleEmoji.value;
    if (pick) selectEmoji(pick);
  }
}

function selectEmoji(emoji: string) {
  recents.value = pushRecent(emoji);
  emit("select", emoji);
  emit("close");
}

function onResize() {
  if (props.open) computePosition();
}

watch(
  () => props.open,
  async (open) => {
    if (!open) return;
    recents.value = loadRecents();
    query.value = "";
    await nextTick();
    computePosition();
    searchInputEl.value?.focus();
  },
);

onMounted(() => {
  window.addEventListener("mousedown", onWindowClick);
  window.addEventListener("keydown", onKey);
  window.addEventListener("resize", onResize);
  window.addEventListener("scroll", onResize, true);
});

onUnmounted(() => {
  window.removeEventListener("mousedown", onWindowClick);
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("resize", onResize);
  window.removeEventListener("scroll", onResize, true);
});
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      ref="panelEl"
      class="fixed z-50 w-72 rounded-xl border border-border bg-card/95 backdrop-blur shadow-xl overflow-hidden flex flex-col"
      :style="{ top: `${position.top}px`, left: `${position.left}px`, maxHeight: `${PANEL_MAX_HEIGHT}px` }"
    >
      <div class="flex items-center gap-2 px-3 py-2 border-b border-border">
        <Search class="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
        <input
          ref="searchInputEl"
          v-model="query"
          type="text"
          placeholder="Search emoji..."
          class="flex-1 text-[13px] bg-transparent border-none focus:outline-none placeholder:text-muted-foreground/50"
        />
        <button
          class="p-1 rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-colors"
          title="Close"
          @click="emit('close')"
        >
          <X class="w-3.5 h-3.5" />
        </button>
      </div>

      <div class="flex-1 overflow-auto p-2">
        <div v-if="!hasQuery && recents.length > 0" class="mb-2">
          <div class="text-[10px] uppercase tracking-wide text-muted-foreground/70 px-1 pb-1">
            Recently used
          </div>
          <div class="grid grid-cols-8 gap-0.5">
            <button
              v-for="e in recents"
              :key="`recent-${e}`"
              class="h-8 w-8 flex items-center justify-center text-[18px] leading-none rounded-md hover:bg-muted hover:scale-110 transition-all duration-150"
              :title="e"
              @click="selectEmoji(e)"
            >{{ e }}</button>
          </div>
        </div>

        <div v-if="!hasQuery" class="text-[10px] uppercase tracking-wide text-muted-foreground/70 px-1 pb-1">
          Common
        </div>

        <div v-if="gridEmojis.length > 0" class="grid grid-cols-8 gap-0.5">
          <button
            v-for="e in gridEmojis"
            :key="e"
            class="h-8 w-8 flex items-center justify-center text-[18px] leading-none rounded-md hover:bg-muted hover:scale-110 transition-all duration-150"
            :title="e"
            @click="selectEmoji(e)"
          >{{ e }}</button>
        </div>
        <div v-else class="py-6 text-center text-[12px] text-muted-foreground">
          No emoji found
        </div>
      </div>
    </div>
  </Teleport>
</template>
