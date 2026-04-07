<script setup lang="ts">
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
        <div class="fixed inset-0 bg-background/80 backdrop-blur-sm" @click="close" />
        <div
          class="fixed inset-y-0 w-[min(92vw,24rem)] bg-background border-foreground overflow-auto"
          :class="side === 'left' ? 'left-0 border-r' : 'right-0 border-l'"
          @click.stop
        >
          <div class="sticky top-0 z-10 flex items-center justify-between px-6 py-4 border-b border-foreground bg-background">
            <slot name="title" />
            <button
              class="text-sm font-mono uppercase tracking-wider px-3 py-1.5 border border-foreground hover:bg-foreground hover:text-background transition-colors"
              @click="close"
            >
              Close
            </button>
          </div>
          <slot />
        </div>
      </div>
    </Transition>
  </Teleport>
</template>
