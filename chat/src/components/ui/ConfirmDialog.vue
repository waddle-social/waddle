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
    <div class="border-b border-border px-6 py-4 flex items-center justify-between">
      <h2 class="text-[16px] font-display font-bold">{{ title }}</h2>
      <button class="p-1.5 rounded-lg hover:bg-muted transition-all duration-200" @click="open = false">
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="px-6 py-5">
      <p class="text-[13px] leading-relaxed text-muted-foreground">{{ message }}</p>
    </div>

    <div class="border-t border-border px-6 py-3.5 flex justify-end gap-2">
      <button
        class="text-[13px] font-medium py-2 px-4 rounded-xl border border-border hover:bg-muted transition-all duration-200"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        class="text-[13px] font-semibold py-2 px-4 rounded-xl transition-all duration-200 disabled:opacity-30"
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
