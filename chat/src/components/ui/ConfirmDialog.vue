<script setup lang="ts">
import { X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  title: string;
  message: string;
  confirmLabel?: string;
  destructive?: boolean;
  loading?: boolean;
}>();

const emit = defineEmits<{
  confirm: [];
}>();
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="flex h-14 items-center justify-between border-b border-border px-4 sm:px-5">
      <h2 class="text-[16px] font-display font-bold">{{ title }}</h2>
      <button class="flex h-8 w-8 items-center justify-center rounded-lg hover:bg-muted transition-all duration-200" @click="open = false">
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="px-4 py-4 sm:px-5">
      <p class="text-[13px] leading-5 text-muted-foreground">{{ message }}</p>
    </div>

    <div class="flex min-h-14 justify-end gap-2 border-t border-border px-4 py-3 sm:px-5">
      <button
        class="h-9 rounded-lg border border-border px-3 text-[13px] font-medium hover:bg-muted transition-all duration-200"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        class="h-9 rounded-lg px-3 text-[13px] font-semibold transition-all duration-200 disabled:opacity-30"
        :class="destructive
          ? 'bg-destructive text-destructive-foreground hover:shadow-[0_0_12px_rgba(239,68,68,0.3)]'
          : 'bg-primary text-primary-foreground hover:shadow-[0_0_12px_var(--glow)]'"
        :disabled="loading"
        @click="emit('confirm')"
      >
        {{ loading ? 'Deleting...' : (confirmLabel ?? 'Confirm') }}
      </button>
    </div>
  </AppDialog>
</template>
