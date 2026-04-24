<script setup lang="ts">
const open = defineModel<boolean>("open", { required: true });

function onBackdropClick() {
  open.value = false;
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape") {
    open.value = false;
  }
}
</script>

<template>
  <Teleport to="body">
    <div v-if="open" class="z-modal fixed inset-0 animate-fade-in" @keydown="onKeydown">
      <div
        class="fixed inset-0 bg-background/60 backdrop-blur-md"
        @click="onBackdropClick"
      />
      <div class="fixed inset-0 flex items-start justify-center overflow-auto p-3 pt-[10vh] sm:p-4 sm:pt-[12vh]">
        <div
          class="relative flex max-h-[min(44rem,calc(100dvh-2rem))] w-full max-w-lg flex-col overflow-hidden glass-panel rounded-lg border border-border shadow-2xl animate-slide-up"
          role="dialog"
          aria-modal="true"
          @click.stop
        >
          <slot />
        </div>
      </div>
    </div>
  </Teleport>
</template>
