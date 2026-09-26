<script setup lang="ts">
import { Dialog } from "@ark-ui/vue/dialog";
import type { DialogOpenChangeDetails } from "@ark-ui/vue/dialog";
import { X } from "lucide-vue-next";

/**
 * Edge-anchored modal panel (Ark Dialog positioned at the left or right
 * edge). Keeps the `side` / `widthClass` / `label` contract and the
 * `title` + default slots; the close button is a `Dialog.CloseTrigger`.
 */
const open = defineModel<boolean>("open", { required: true });
const props = defineProps<{ side: "left" | "right"; widthClass?: string; label?: string }>();

function onOpenChange(details: DialogOpenChangeDetails) {
  open.value = details.open;
}
</script>

<template>
  <Dialog.Root
    :open="open"
    lazy-mount
    unmount-on-exit
    @open-change="onOpenChange"
  >
    <Teleport to="body">
      <Dialog.Backdrop class="app-drawer__backdrop z-modal fixed inset-0 bg-background/70 data-[state=open]:animate-fade-in" />
      <Dialog.Positioner
        class="app-drawer__positioner z-modal fixed top-0 flex h-[100dvh]"
        :class="side === 'left' ? 'left-0' : 'right-0'"
      >
        <Dialog.Content
          class="app-drawer flex h-[100dvh] max-w-full flex-col border-border bg-card text-card-foreground shadow-[var(--shadow-floating)] outline-none data-[state=open]:animate-fade-in"
          :class="[
            props.widthClass ?? 'w-[var(--chat-drawer-width)]',
            side === 'left' ? 'border-r' : 'border-l',
          ]"
          :data-side="side"
          :aria-label="props.label ?? 'Drawer'"
        >
          <div class="flex h-14 flex-shrink-0 items-center justify-between border-b border-border px-4 py-0">
            <slot name="title" />
            <Dialog.CloseTrigger as-child>
              <button class="chat-icon-button hover:bg-muted" type="button" aria-label="Close drawer">
                <X class="h-4 w-4" />
              </button>
            </Dialog.CloseTrigger>
          </div>
          <div class="chat-pane-scroll min-h-0 flex-1">
            <slot />
          </div>
        </Dialog.Content>
      </Dialog.Positioner>
    </Teleport>
  </Dialog.Root>
</template>
