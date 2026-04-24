<script setup lang="ts">
import { X } from "lucide-vue-next";

const open = defineModel<boolean>("open", { required: true });
const props = defineProps<{ side: "left" | "right"; widthClass?: string; label?: string }>();

function close() {
  open.value = false;
}
</script>

<template>
  <Teleport to="body">
    <Transition name="drawer">
      <div
        v-if="open"
        class="z-modal fixed top-0 left-0 right-0 h-[100dvh]"
        @keydown.esc="close"
      >
        <div
          class="absolute inset-0 bg-background/60 backdrop-blur-md animate-fade-in"
          aria-hidden="true"
          @click="close"
        />
        <div
          class="absolute top-0 h-[100dvh] glass-panel border-border shadow-2xl flex flex-col"
          :class="[
            props.widthClass ?? 'w-[var(--chat-drawer-width)]',
            side === 'left' ? 'left-0 border-r' : 'right-0 border-l',
          ]"
          role="dialog"
          aria-modal="true"
          :aria-label="props.label ?? 'Drawer'"
          @click.stop
        >
          <div class="h-14 flex-shrink-0 flex items-center justify-between px-4 py-0 border-b border-border glass-panel">
            <slot name="title" />
            <button
              class="chat-icon-button hover:bg-muted"
              type="button"
              aria-label="Close drawer"
              @click="close"
            >
              <X class="w-4 h-4" />
            </button>
          </div>
          <div class="chat-pane-scroll flex-1 min-h-0 overflow-auto">
            <slot />
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>
