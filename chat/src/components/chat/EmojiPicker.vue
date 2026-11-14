<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch, type CSSProperties } from "vue";
import { css } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";
import { Search, SearchX, X } from "lucide-vue-next";
import { searchEmoji } from "@/lib/emoji";
import { pickerSearch } from "./composer-classes";

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
    /** "react" labels picks as reactions; "insert" as composer insertions. */
    purpose?: "react" | "insert";
  }>(),
  { variant: "popover", purpose: "react" },
);

const emit = defineEmits<{
  select: [emoji: string];
  /** `outside` = dismissed by a pointer elsewhere, which already moves focus. */
  close: [reason: "escape" | "outside" | "button"];
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

const panelLabel = computed(() => (props.purpose === "insert" ? "Insert an emoji" : "Choose a reaction"));

function emojiLabel(emoji: string): string {
  return props.purpose === "insert" ? `Insert ${emoji}` : `React with ${emoji}`;
}

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
  emit("close", "outside");
}

function onKey(event: KeyboardEvent) {
  if (event.key === "Escape") emit("close", "escape");
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

const panel = css({ display: "flex", flexDirection: "column", overflow: "hidden", color: "fg" });
const panelPopover = css({
  position: "fixed",
  zIndex: "dropdown",
  width: "min(352px, calc(100vw - 32px))",
  maxHeight: "384px",
  bg: "surface",
  border: "1px solid {colors.border}",
  borderRadius: "panel",
  boxShadow: "overlay",
  animation: "wd-pop {durations.base} {easings.spring.snappy} backwards",
});
const panelSheet = css({ width: "100%", maxHeight: "60vh" });
const body = css({ flex: 1, overflow: "auto", p: "2" });
const sectionLabel = css({ display: "flex", alignItems: "center", height: "24px", px: "1", textStyle: "eyebrow" });
const gridClass = css({ display: "grid", gap: "1", mb: "2" });
const gridSheet = css({ gridTemplateColumns: "repeat(7, minmax(0, 1fr))" });
const gridPopover = css({ gridTemplateColumns: "repeat(8, minmax(0, 1fr))" });
const cell = css({
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  border: 0,
  borderRadius: "sm",
  bg: "transparent",
  lineHeight: 1,
  cursor: "pointer",
  transition: "background-color {durations.fast} {easings.out}",
  _hover: { bg: "state.hover" },
  _active: { bg: "state.press" },
});
const cellSheet = css({ height: "48px", fontSize: "28px" });
const cellPopover = css({ width: "36px", height: "36px", fontSize: "22px" });
</script>

<template>
  <Teleport to="body" :disabled="variant !== 'popover'">
    <div
      v-if="open"
      ref="panelEl"
      :role="variant === 'popover' ? 'dialog' : 'group'"
      :aria-label="panelLabel"
      :class="[panel, variant === 'popover' ? panelPopover : panelSheet]"
      :style="variant === 'popover' ? popoverStyle : undefined"
      @pointerdown.stop
    >
      <div :class="pickerSearch.bar">
        <div :class="pickerSearch.box">
          <Search :class="pickerSearch.icon" aria-hidden="true" />
          <input
            ref="searchInputEl"
            v-model="query"
            type="text"
            placeholder="Search emoji…"
            aria-label="Search emoji"
            :class="pickerSearch.input"
          />
          <button
            type="button"
            :class="[iconbtn({ size: 'sm' }), pickerSearch.close]"
            aria-label="Close emoji picker"
            @click="emit('close', 'button')"
          >
            <X aria-hidden="true" />
          </button>
        </div>
      </div>

      <div :class="body">
        <template v-if="searchResults.length > 0">
          <div :class="sectionLabel">Results</div>
          <div :class="[gridClass, variant === 'sheet' ? gridSheet : gridPopover]">
            <button
              v-for="e in searchResults"
              :key="`s-${e}`"
              type="button"
              :class="[cell, variant === 'sheet' ? cellSheet : cellPopover]"
              :aria-label="emojiLabel(e)"
              @click="onSelect(e)"
            >{{ e }}</button>
          </div>
        </template>
        <template v-else-if="query.trim().length >= 2">
          <div :class="pickerSearch.state">
            <SearchX aria-hidden="true" />
            <span>No emoji found</span>
          </div>
        </template>
        <template v-else>
          <template v-if="recents.length > 0">
            <div :class="sectionLabel">Recent</div>
            <div :class="[gridClass, variant === 'sheet' ? gridSheet : gridPopover]">
              <button
                v-for="e in recents"
                :key="`r-${e}`"
                type="button"
              :class="[cell, variant === 'sheet' ? cellSheet : cellPopover]"
                :aria-label="emojiLabel(e)"
                @click="onSelect(e)"
              >{{ e }}</button>
            </div>
          </template>
          <div :class="sectionLabel">Common</div>
          <div :class="[gridClass, variant === 'sheet' ? gridSheet : gridPopover]">
            <button
              v-for="e in COMMON_EMOJIS"
              :key="`c-${e}`"
              type="button"
              :class="[cell, variant === 'sheet' ? cellSheet : cellPopover]"
              :aria-label="emojiLabel(e)"
              @click="onSelect(e)"
            >{{ e }}</button>
          </div>
        </template>
      </div>
    </div>
  </Teleport>
</template>
