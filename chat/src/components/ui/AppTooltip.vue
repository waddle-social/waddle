<script setup lang="ts">
import { computed } from "vue";
import { Tooltip } from "@ark-ui/vue/tooltip";
import { css } from "styled-system/css";

type TooltipPlacement =
  | "top"
  | "top-start"
  | "top-end"
  | "bottom"
  | "bottom-start"
  | "bottom-end"
  | "left"
  | "left-start"
  | "left-end"
  | "right"
  | "right-start"
  | "right-end";

/**
 * Hover / focus tooltip on the slotted trigger element (Ark Tooltip).
 *
 * The default slot must be a single element — usually the `<button>` the
 * tooltip describes; Ark merges its trigger attributes onto it (`asChild`),
 * so keep the `aria-label` on the button: the tooltip is a visual hint,
 * not the accessible name. An empty `label` disables the tooltip and
 * renders the trigger untouched.
 */
const props = withDefaults(
  defineProps<{
    label?: string | null;
    placement?: TooltipPlacement;
    /**
     * When the trigger is also another Ark trigger (a Menu or Popover
     * button), pass that component's trigger id here so both anatomies
     * agree on the element's `id` instead of the tooltip overwriting it.
     */
    triggerId?: string;
  }>(),
  { label: "", placement: "top", triggerId: undefined },
);

const contentClass = css({
  zIndex: "tooltip",
  pointerEvents: "none",
  maxWidth: "280px",
  padding: "{spacing.1.5} {spacing.2.5}",
  borderRadius: "sm",
  bg: "fg",
  color: "bg",
  font: "600 12px/1.35 {fonts.body}",
  boxShadow: "elev.2",
  animation: "wd-fade {durations.fast} {easings.out} backwards",
});

const positioning = computed(() => ({ placement: props.placement, gutter: 6 }));
const disabled = computed(() => !props.label);
const ids = computed(() => (props.triggerId ? { trigger: props.triggerId } : undefined));
</script>

<template>
  <Tooltip.Root
    :disabled="disabled"
    :open-delay="300"
    :close-delay="100"
    :positioning="positioning"
    :ids="ids"
    lazy-mount
    unmount-on-exit
  >
    <Tooltip.Trigger as-child>
      <slot />
    </Tooltip.Trigger>
    <Teleport to="body">
      <Tooltip.Positioner>
        <Tooltip.Content :class="contentClass">
          {{ label }}
        </Tooltip.Content>
      </Tooltip.Positioner>
    </Teleport>
  </Tooltip.Root>
</template>
