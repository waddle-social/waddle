<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { $desktopToolbarSuspensionEpoch } from "@/stores/message-toolbar";
import { css } from "styled-system/css";
import { chip, reactions as reactionsRecipe } from "styled-system/recipes";
import { formatReactors, isReactor, reactionAriaLabel } from "./message-reaction-chips";

const props = defineProps<{
  reactions: Record<string, readonly string[]>;
  currentUser?: string;
}>();

const emit = defineEmits<{
  react: [emoji: string];
}>();

function userIsReactor(nicks: readonly string[]): boolean {
  return isReactor(nicks, props.currentUser);
}

// Reaction tooltip is teleported to <body> so that pane-level
// `overflow-x: hidden` (`.chat-pane-scroll`) cannot clip it. The chip stays
// in place; on hover/focus we capture the chip's bounding rect and render a
// fixed-position panel above it. Touch devices use the action sheet instead;
// keyboard reveal is gated on `:focus-visible` so a mouse click does not
// briefly flash the tooltip alongside the just-applied reaction.
const hoveredReaction = ref<{
  emoji: string;
  nicks: readonly string[];
  rect: DOMRect;
} | null>(null);

const REACTION_TOOLTIP_HALF_WIDTH = 144; // half of the tooltip's 288px max width
const REACTION_TOOLTIP_GAP = 8;

function showReactionFromPointer(emoji: string, nicks: readonly string[], event: PointerEvent) {
  // Touch: skip — touch devices use the action sheet for reactions and the
  // synthesized pointerenter has no matching pointerleave on tap.
  // Mouse + pen (stylus): show — both fire pointerleave reliably and
  // benefit from the hover affordance.
  if (event.pointerType !== "mouse" && event.pointerType !== "pen") return;
  const target = event.currentTarget;
  if (!(target instanceof HTMLElement)) return;
  hoveredReaction.value = { emoji, nicks, rect: target.getBoundingClientRect() };
}

function showReactionFromFocus(emoji: string, nicks: readonly string[], event: FocusEvent) {
  const target = event.currentTarget;
  if (!(target instanceof HTMLElement)) return;
  if (!target.matches(":focus-visible")) return;
  hoveredReaction.value = { emoji, nicks, rect: target.getBoundingClientRect() };
}

function hideReactionTooltip(emoji: string) {
  if (hoveredReaction.value?.emoji === emoji) hoveredReaction.value = null;
}

const reactionTooltipStyle = computed(() => {
  const hover = hoveredReaction.value;
  if (!hover) return null;
  const viewportWidth = typeof window === "undefined" ? hover.rect.right + REACTION_TOOLTIP_HALF_WIDTH : window.innerWidth;
  const wantedCenter = hover.rect.left + hover.rect.width / 2;
  const minCenter = REACTION_TOOLTIP_HALF_WIDTH + REACTION_TOOLTIP_GAP;
  const maxCenter = viewportWidth - REACTION_TOOLTIP_HALF_WIDTH - REACTION_TOOLTIP_GAP;
  const clampedCenter = Math.max(minCenter, Math.min(maxCenter, wantedCenter));
  return {
    top: `${hover.rect.top - REACTION_TOOLTIP_GAP}px`,
    left: `${clampedCenter}px`,
  };
});

function onReactionTooltipScroll() {
  hoveredReaction.value = null;
}

watch(hoveredReaction, (next) => {
  if (typeof window === "undefined") return;
  if (next) {
    window.addEventListener("scroll", onReactionTooltipScroll, true);
  }
  else {
    window.removeEventListener("scroll", onReactionTooltipScroll, true);
  }
});

// Timeline-wide suspensions (e.g. a modal opening) dismiss any hovered
// tooltip alongside the toolbar/picker surfaces the parent card closes.
const desktopToolbarSuspensionEpoch = useStore($desktopToolbarSuspensionEpoch);
watch(
  () => desktopToolbarSuspensionEpoch.value,
  () => {
    hoveredReaction.value = null;
  },
);

const rowClass = reactionsRecipe();
const chipClass = chip();
const tooltipClass = css({
  position: "fixed",
  zIndex: "tooltip",
  pointerEvents: "none",
  display: "flex",
  flexDirection: "column",
  alignItems: "center",
  gap: "0.5",
  maxWidth: "288px",
  translate: "-50% -100%",
  px: "2",
  py: "1.5",
  borderRadius: "sm",
  border: "1px solid {colors.border}",
  bg: "surface",
  color: "fg",
  boxShadow: "elev.2",
  "& > :first-child": { fontSize: "lg", lineHeight: 1 },
  "& > :last-child": { textStyle: "caption", color: "fg.muted", textAlign: "center" },
});

onBeforeUnmount(() => {
  if (typeof window === "undefined") return;
  window.removeEventListener("scroll", onReactionTooltipScroll, true);
});
</script>

<template>
  <!-- Existing reactions (inline, always visible when present) -->
  <div :class="rowClass">
    <button
      v-for="(nicks, emoji) in reactions"
      :key="emoji"
      type="button"
      :class="chipClass"
      :aria-label="reactionAriaLabel(emoji, nicks)"
      :aria-pressed="userIsReactor(nicks)"
      @click="emit('react', emoji)"
      @pointerenter="(event) => showReactionFromPointer(emoji, nicks, event)"
      @pointerleave="() => hideReactionTooltip(emoji)"
      @focusin="(event) => showReactionFromFocus(emoji, nicks, event)"
      @focusout="() => hideReactionTooltip(emoji)"
    >
      <span>{{ emoji }}</span>
      <span class="wd-chip__n">{{ nicks.length }}</span>
    </button>
  </div>

  <!-- Reaction tooltip: teleported so it escapes the pane's
       `overflow-x: hidden`. Anchored above the hovered chip via the
       captured rect. -->
  <Teleport to="body">
    <div
      v-if="hoveredReaction && reactionTooltipStyle"
      aria-hidden="true"
      :class="tooltipClass"
      :style="reactionTooltipStyle"
    >
      <span aria-hidden="true">{{ hoveredReaction.emoji }}</span>
      <span>{{ formatReactors(hoveredReaction.nicks) }}</span>
    </div>
  </Teleport>
</template>
