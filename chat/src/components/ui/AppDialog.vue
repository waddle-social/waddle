<script setup lang="ts">
import { computed } from "vue";
import { Dialog } from "@ark-ui/vue/dialog";
import type { DialogOpenChangeDetails } from "@ark-ui/vue/dialog";
import { css, cx } from "styled-system/css";
import { dialog, scrim } from "styled-system/recipes";

/**
 * Base modal dialog (Ark Dialog). Portalled to `<body>`, lazily mounted,
 * traps focus, locks scroll, closes on Escape and backdrop click.
 *
 * The surface is the preset `dialog` recipe without its padding: consumers
 * render their own header/body/footer inside (or use `dialogParts` from
 * `@/ui/part-classes` for the board layout). `size` picks the width:
 * sm 440, md 520 (default), lg 640px. `labelledBy` points
 * `aria-labelledby` at the consumer's heading id.
 */
const open = defineModel<boolean>("open", { required: true });

const props = withDefaults(
  defineProps<{
    labelledBy?: string;
    role?: "dialog" | "alertdialog";
    size?: "sm" | "md" | "lg";
  }>(),
  { labelledBy: undefined, role: "dialog", size: "md" },
);

const positionerClass = css({
  position: "fixed",
  inset: 0,
  zIndex: "dialog",
  display: "flex",
  alignItems: "flex-start",
  justifyContent: "center",
  overflow: "auto",
  padding: "12vh {spacing.4} {spacing.4}",
  _narrow: { paddingTop: "8vh" },
});
const surfaceClass = css({
  "--pad": "0px",
  position: "relative",
  gap: 0,
  maxHeight: "min(44rem, calc(100dvh - 2rem))",
  overflow: "hidden",
  color: "fg",
  outline: "none",
});
const contentClass = computed(() =>
  cx(dialog({ size: props.size === "md" ? undefined : props.size }), surfaceClass, "app-dialog"),
);

function onOpenChange(details: DialogOpenChangeDetails) {
  open.value = details.open;
}
</script>

<template>
  <Dialog.Root :open="open" :role="role" lazy-mount unmount-on-exit @open-change="onOpenChange">
    <Teleport to="body">
      <Dialog.Backdrop :class="scrim()" />
      <Dialog.Positioner :class="positionerClass">
        <Dialog.Content
          :class="contentClass"
          v-bind="labelledBy ? { 'aria-labelledby': labelledBy } : {}"
        >
          <slot />
        </Dialog.Content>
      </Dialog.Positioner>
    </Teleport>
  </Dialog.Root>
</template>
