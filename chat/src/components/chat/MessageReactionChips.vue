<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { $desktopToolbarSuspensionEpoch } from "@/stores/message-toolbar";
import { formatReactors, isReactor, reactionAriaLabel, reactionWarmthClass } from "./message-reaction-chips";

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

const REACTION_TOOLTIP_HALF_WIDTH = 144; // matches max-w-[18rem]
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

onBeforeUnmount(() => {
  if (typeof window === "undefined") return;
  window.removeEventListener("scroll", onReactionTooltipScroll, true);
});
</script>

<template>
  <!-- Existing reactions (inline, always visible when present) -->
  <div class="chat-message-reactions flex flex-wrap gap-1">
    <button
      v-for="(nicks, emoji) in reactions"
      :key="emoji"
      type="button"
      class="chat-reaction-chip type-caption inline-flex h-7 items-center gap-1 px-2 rounded-lg"
      :class="[
        userIsReactor(nicks) ? 'chat-reaction-chip--self' : '',
        reactionWarmthClass(nicks.length),
      ]"
      :aria-label="reactionAriaLabel(emoji, nicks)"
      :aria-pressed="userIsReactor(nicks)"
      @click="emit('react', emoji)"
      @pointerenter="(event) => showReactionFromPointer(emoji, nicks, event)"
      @pointerleave="() => hideReactionTooltip(emoji)"
      @focusin="(event) => showReactionFromFocus(emoji, nicks, event)"
      @focusout="() => hideReactionTooltip(emoji)"
    >
      <span class="chat-reaction-chip__glyph">{{ emoji }}</span>
      <span class="type-meta type-numeric chat-reaction-chip__count">{{ nicks.length }}</span>
    </button>
  </div>

  <!-- Reaction tooltip: teleported so it escapes `.chat-pane-scroll`'s
       `overflow-x: hidden`. Anchored above the hovered chip via the captured
       rect. -->
  <Teleport to="body">
    <div
      v-if="hoveredReaction && reactionTooltipStyle"
      aria-hidden="true"
      class="chat-reaction-tooltip pointer-events-none fixed z-popover flex max-w-[18rem] -translate-x-1/2 -translate-y-full flex-col items-center gap-0.5 rounded-md border border-border bg-popover px-2 py-1.5 text-popover-foreground shadow-md"
      :style="reactionTooltipStyle"
    >
      <span class="text-lg leading-none" aria-hidden="true">{{ hoveredReaction.emoji }}</span>
      <span class="type-meta text-center text-muted-foreground">
        {{ formatReactors(hoveredReaction.nicks) }}
      </span>
    </div>
  </Teleport>
</template>
