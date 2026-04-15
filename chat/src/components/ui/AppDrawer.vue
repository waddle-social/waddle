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
        <div class="fixed inset-0 bg-background/70 backdrop-blur-sm animate-fade-in" @click="close" />
        <div
          class="fixed inset-y-0 w-[min(88vw,22rem)] bg-card border-border overflow-auto shadow-lg"
          :class="side === 'left' ? 'left-0 border-r' : 'right-0 border-l'"
          @click.stop
        >
          <div class="sticky top-0 z-10 flex items-center justify-between px-4 py-3 border-b border-border bg-card">
            <slot name="title" />
            <button
              class="p-1 rounded-md hover:bg-muted transition-colors"
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
