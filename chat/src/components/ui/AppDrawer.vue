<script setup lang="ts">
import { X } from "lucide-vue-next";

const open = defineModel<boolean>("open", { required: true });
defineProps<{ side: "left" | "right" }>();

function close() {
  open.value = false;
}
</script>

<template>
  <Teleport to="body">
    <Transition name="drawer">
      <div v-if="open" class="fixed inset-0 z-50">
        <div class="fixed inset-0 bg-background/60 backdrop-blur-md animate-fade-in" @click="close" />
        <div
          class="fixed inset-y-0 w-[min(88vw,22rem)] glass-panel border-border overflow-auto shadow-2xl"
          :class="side === 'left' ? 'left-0 border-r' : 'right-0 border-l'"
          @click.stop
        >
          <div class="sticky top-0 z-10 flex items-center justify-between px-4 py-3 border-b border-border glass-panel">
            <slot name="title" />
            <button
              class="p-1.5 rounded-lg hover:bg-muted transition-all duration-200"
              @click="close"
            >
              <X class="w-4 h-4" />
            </button>
          </div>
          <slot />
        </div>
      </div>
    </Transition>
  </Teleport>
</template>
