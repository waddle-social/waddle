<script setup lang="ts">
import { X } from "lucide-vue-next";

const open = defineModel<boolean>("open", { required: true });
const props = defineProps<{ side: "left" | "right"; widthClass?: string }>();

function close() {
  open.value = false;
}
</script>

<template>
  <Teleport to="body">
    <Transition name="drawer">
      <div v-if="open" class="fixed top-0 left-0 right-0 z-50 h-[100dvh]">
        <div class="absolute inset-0 bg-background/60 backdrop-blur-md animate-fade-in" @click="close" />
        <div
          class="absolute top-0 h-[100dvh] glass-panel border-border shadow-2xl flex flex-col"
          :class="[
            props.widthClass ?? 'w-[min(88vw,22rem)]',
            side === 'left' ? 'left-0 border-r' : 'right-0 border-l',
          ]"
          @click.stop
        >
          <div class="flex-shrink-0 flex items-center justify-between px-4 py-3 border-b border-border glass-panel">
            <slot name="title" />
            <button
              class="p-1.5 rounded-lg hover:bg-muted transition-all duration-200"
              @click="close"
            >
              <X class="w-4 h-4" />
            </button>
          </div>
          <div class="flex-1 min-h-0 overflow-auto">
            <slot />
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>
