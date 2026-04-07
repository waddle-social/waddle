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
    <div v-if="open" class="fixed inset-0 z-50" @keydown="onKeydown">
      <div
        class="fixed inset-0 bg-background/80 backdrop-blur-sm"
        @click="onBackdropClick"
      />
      <div class="fixed inset-0 flex items-start justify-center pt-20 p-4 overflow-auto">
        <div
          class="relative w-full max-w-2xl bg-background border-2 border-foreground"
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
