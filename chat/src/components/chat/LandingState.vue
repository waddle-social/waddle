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
  <div class="min-h-screen flex items-center justify-center p-6 bg-background">
    <div class="w-full max-w-sm animate-slide-up">
      <div class="bg-card rounded-lg border border-border p-6 shadow-sm">
        <div class="w-10 h-10 rounded-lg bg-primary/10 flex items-center justify-center mb-4">
          <span class="text-xl">🐧</span>
        </div>
        <h1 class="text-[18px] font-semibold tracking-tight mb-1.5">
          {{ title }}
        </h1>
        <p v-if="copy" class="text-[13px] text-muted-foreground leading-relaxed mb-4">
          {{ copy }}
        </p>
        <button
          v-if="actionLabel"
          class="w-full py-2 px-3 text-[13px] font-medium rounded-md bg-primary text-primary-foreground hover:opacity-90 transition-all"
          @click="$emit('action')"
        >
          {{ actionLabel }}
        </button>
      </div>
    </div>
  </div>
</template>
