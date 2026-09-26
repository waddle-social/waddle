<script setup lang="ts">
import { computed } from "vue";
import { Tooltip } from "@ark-ui/vue/tooltip";

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
        <Tooltip.Content
          class="app-tooltip z-[calc(var(--z-lightbox)+1)] pointer-events-none max-w-xs rounded-lg bg-foreground px-2.5 py-1.5 text-[12px] font-semibold leading-snug text-background shadow-[var(--shadow-floating)] data-[state=open]:animate-fade-in"
        >
          {{ label }}
        </Tooltip.Content>
      </Tooltip.Positioner>
    </Teleport>
  </Tooltip.Root>
</template>
