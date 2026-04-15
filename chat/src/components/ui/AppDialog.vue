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
    <div v-if="open" class="fixed inset-0 z-50 animate-fade-in" @keydown="onKeydown">
      <div
        class="fixed inset-0 bg-background/60 backdrop-blur-md"
        @click="onBackdropClick"
      />
      <div class="fixed inset-0 flex items-start justify-center pt-[15vh] p-4 overflow-auto">
        <div
          class="relative w-full max-w-lg glass-panel rounded-2xl border border-border shadow-2xl animate-slide-up"
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
