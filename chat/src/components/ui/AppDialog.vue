<script setup lang="ts">
import { Dialog } from "@ark-ui/vue/dialog";
import type { DialogOpenChangeDetails } from "@ark-ui/vue/dialog";

/**
 * Base modal dialog (Ark Dialog). Portalled to `<body>`, lazily mounted,
 * traps focus, locks scroll, closes on Escape and backdrop click.
 *
 * Consumers render their own header/body/footer inside; a close button
 * can be a plain `<button>` that sets `open = false`, or `Dialog.CloseTrigger`.
 * `labelledBy` points `aria-labelledby` at the consumer's heading id.
 */
const open = defineModel<boolean>("open", { required: true });

withDefaults(
  defineProps<{
    labelledBy?: string;
    role?: "dialog" | "alertdialog";
  }>(),
  { labelledBy: undefined, role: "dialog" },
);

function onOpenChange(details: DialogOpenChangeDetails) {
  open.value = details.open;
}
</script>

<template>
  <Dialog.Root :open="open" :role="role" lazy-mount unmount-on-exit @open-change="onOpenChange">
    <Teleport to="body">
      <Dialog.Backdrop class="app-dialog__backdrop z-modal fixed inset-0 bg-background/70 data-[state=open]:animate-fade-in" />
      <Dialog.Positioner
        class="app-dialog__positioner z-modal fixed inset-0 flex items-start justify-center overflow-auto p-3 pt-[10vh] sm:p-4 sm:pt-[12vh]"
      >
        <Dialog.Content
          class="app-dialog relative flex max-h-[min(44rem,calc(100dvh-2rem))] w-full max-w-lg flex-col overflow-hidden rounded-lg border border-border bg-card text-card-foreground shadow-[var(--shadow-floating)] outline-none data-[state=open]:animate-slide-up"
          v-bind="labelledBy ? { 'aria-labelledby': labelledBy } : {}"
        >
          <slot />
        </Dialog.Content>
      </Dialog.Positioner>
    </Teleport>
  </Dialog.Root>
</template>
