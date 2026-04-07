<script setup lang="ts">
defineProps<{
  title: string;
  copy?: string;
  actionLabel?: string;
}>();

defineEmits<{
  action: [];
}>();
</script>

<template>
  <div class="min-h-screen flex items-center justify-center p-6">
    <div class="w-full max-w-md border-2 border-foreground bg-background p-8">
      <h1 class="text-2xl font-mono font-bold uppercase tracking-wider mb-4">
        {{ title }}
      </h1>
      <p v-if="copy" class="text-sm font-mono text-muted-foreground mb-6 leading-relaxed">
        {{ copy }}
      </p>
      <button
        v-if="actionLabel"
        class="w-full py-3 px-4 font-mono text-sm uppercase tracking-wider border border-foreground bg-foreground text-background hover:bg-foreground/90 transition-colors"
        @click="$emit('action')"
      >
        {{ actionLabel }}
      </button>
    </div>
  </div>
</template>
