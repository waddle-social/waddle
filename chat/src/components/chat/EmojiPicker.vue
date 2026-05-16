<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch, type CSSProperties } from "vue";
import { Search, SearchX, X } from "lucide-vue-next";
import { searchEmoji } from "@/lib/emoji";

const props = withDefaults(
  defineProps<{
    open: boolean;
    anchorEl?: HTMLElement | null;
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
const popoverStyle = ref<CSSProperties>({});

// Rough upper bound matching the popover shell. Used before the panel has a
// measured height so the first frame picks the right side instead of flashing.
const ESTIMATED_PANEL_HEIGHT = 384;
const ESTIMATED_PANEL_WIDTH = 352;
const VIEWPORT_MARGIN = 8;
const PANEL_GAP = 8;

function updatePopoverPosition() {
  if (props.variant !== "popover") return;
  const anchor = props.anchorEl;
  const panel = panelEl.value;
  if (!anchor || typeof window === "undefined") return;

  const anchorRect = anchor.getBoundingClientRect();
  const panelHeight = panel.offsetHeight || ESTIMATED_PANEL_HEIGHT;
  const panelWidth = panel.offsetWidth || ESTIMATED_PANEL_WIDTH;
  const spaceBelow = window.innerHeight - anchorRect.bottom - VIEWPORT_MARGIN;
  const spaceAbove = anchorRect.top - VIEWPORT_MARGIN;
  const flipAbove = spaceBelow < panelHeight + PANEL_GAP && spaceAbove > spaceBelow;
  const maxLeft = Math.max(VIEWPORT_MARGIN, window.innerWidth - panelWidth - VIEWPORT_MARGIN);
  const left = Math.min(Math.max(anchorRect.right - panelWidth, VIEWPORT_MARGIN), maxLeft);
  const maxTop = Math.max(VIEWPORT_MARGIN, window.innerHeight - panelHeight - VIEWPORT_MARGIN);
  const top = flipAbove
    ? Math.max(VIEWPORT_MARGIN, anchorRect.top - panelHeight - PANEL_GAP)
    : Math.min(maxTop, anchorRect.bottom + PANEL_GAP);

  popoverStyle.value = {
    left: `${Math.round(left)}px`,
    top: `${Math.round(top)}px`,
  };
}

function onSelect(emoji: string) {
  saveRecent(emoji);
  emit("select", emoji);
}

function onWindowPointer(event: PointerEvent | MouseEvent) {
  if (!panelEl.value) return;
  const target = event.target as Node | null;
  if (!target) return;
  if (panelEl.value.contains(target)) return;
  if (props.anchorEl?.contains(target)) return;
  emit("close");
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
  window.addEventListener("resize", updatePopoverPosition);
  window.addEventListener("scroll", updatePopoverPosition, true);
}

function detachWindowListeners() {
  if (typeof window === "undefined") return;
  window.removeEventListener("pointerdown", onWindowPointer, true);
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("resize", updatePopoverPosition);
  window.removeEventListener("scroll", updatePopoverPosition, true);
}

watch(
  () => props.open,
  (open) => {
    if (open) {
      loadRecents();
      attachWindowListeners();
      query.value = "";
      void nextTick().then(() => {
        updatePopoverPosition();
        searchInputEl.value?.focus();
      });
    } else {
      detachWindowListeners();
    }
  },
  { immediate: true },
);

watch(
  () => [query.value, searchResults.value.length, recents.value.length],
  () => {
    if (!props.open || props.variant !== "popover") return;
    void nextTick().then(updatePopoverPosition);
  },
);

onBeforeUnmount(detachWindowListeners);
</script>

<template>
  <Teleport to="body" :disabled="variant !== 'popover'">
    <div
      v-if="open"
      ref="panelEl"
      :role="variant === 'popover' ? 'dialog' : 'group'"
      aria-label="Choose a reaction"
      :class="[
        'flex flex-col overflow-hidden',
        variant === 'popover'
          ? 'z-popover fixed w-[var(--chat-reaction-popover-width)] glass-panel border border-border rounded-lg shadow-2xl animate-fade-in max-h-96'
          : 'w-full max-h-[60vh]',
      ]"
      :style="variant === 'popover' ? popoverStyle : undefined"
      @pointerdown.stop
    >
      <div class="border-b border-border p-2">
        <div class="relative min-w-0 rounded-lg bg-muted/60 transition-colors focus-within:bg-muted/75 focus-within:ring-1 focus-within:ring-inset focus-within:ring-primary/20">
          <Search
            class="pointer-events-none absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            aria-hidden="true"
          />
          <input
            ref="searchInputEl"
            v-model="query"
            type="text"
            placeholder="Search emoji…"
            aria-label="Search emoji"
            class="type-field min-h-10 w-full rounded-lg border-none bg-transparent py-2.5 pl-9 pr-11 placeholder:text-muted-foreground/40 focus:outline-none"
          />
          <button
            type="button"
            class="absolute right-1.5 top-1/2 flex h-7 w-7 -translate-y-1/2 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
            aria-label="Close emoji picker"
            @click="emit('close')"
          >
            <X class="w-3.5 h-3.5" aria-hidden="true" />
          </button>
        </div>
      </div>

      <div class="flex-1 overflow-auto p-2">
        <template v-if="searchResults.length > 0">
          <div class="type-section-label flex h-6 items-center px-1 text-muted-foreground/70">Results</div>
          <div :class="['grid gap-1', variant === 'sheet' ? 'grid-cols-7' : 'grid-cols-8']">
            <button
              v-for="e in searchResults"
              :key="`s-${e}`"
              type="button"
              :class="[
                'flex items-center justify-center rounded-lg hover:bg-muted active:bg-muted transition-all duration-150',
                variant === 'sheet' ? 'type-emoji-sheet h-12' : 'type-emoji-picker h-9 w-9',
              ]"
              :aria-label="`React with ${e}`"
              @click="onSelect(e)"
            >{{ e }}</button>
          </div>
        </template>
        <template v-else-if="query.trim().length >= 2">
          <!-- Empty search state — SearchX glyph stacked above the
               caption matches the iter-47 channel-search empty state
               so every "your query had no hits" surface in the app
               speaks one visual language. -->
          <div class="flex h-24 flex-col items-center justify-center gap-1.5 rounded-lg text-center">
            <SearchX class="h-5 w-5 text-muted-foreground/60" aria-hidden="true" />
            <span class="type-caption text-muted-foreground">No emoji found</span>
          </div>
        </template>
        <template v-else>
          <template v-if="recents.length > 0">
            <div class="type-section-label flex h-6 items-center px-1 text-muted-foreground/70">Recent</div>
            <div :class="['grid gap-1 mb-2', variant === 'sheet' ? 'grid-cols-7' : 'grid-cols-8']">
              <button
                v-for="e in recents"
                :key="`r-${e}`"
                type="button"
                :class="[
                  'flex items-center justify-center rounded-lg hover:bg-muted active:bg-muted transition-all duration-150',
                  variant === 'sheet' ? 'type-emoji-sheet h-12' : 'type-emoji-picker h-9 w-9',
                ]"
                :aria-label="`React with ${e}`"
                @click="onSelect(e)"
              >{{ e }}</button>
            </div>
          </template>
          <div class="type-section-label flex h-6 items-center px-1 text-muted-foreground/70">Common</div>
          <div :class="['grid gap-1', variant === 'sheet' ? 'grid-cols-7' : 'grid-cols-8']">
            <button
              v-for="e in COMMON_EMOJIS"
              :key="`c-${e}`"
              type="button"
              :class="[
                'flex items-center justify-center rounded-lg hover:bg-muted active:bg-muted transition-all duration-150',
                variant === 'sheet' ? 'type-emoji-sheet h-12' : 'type-emoji-picker h-9 w-9',
              ]"
              :aria-label="`React with ${e}`"
              @click="onSelect(e)"
            >{{ e }}</button>
          </div>
        </template>
      </div>
    </div>
  </Teleport>
</template>
