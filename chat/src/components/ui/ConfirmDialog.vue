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
    <div class="border-b border-border px-5 py-4 flex items-center justify-between">
      <h2 class="text-[15px] font-semibold">{{ title }}</h2>
      <button class="p-1 rounded-md hover:bg-muted transition-colors" @click="open = false">
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="px-5 py-4">
      <p class="text-[13px] leading-relaxed text-muted-foreground">{{ message }}</p>
    </div>

    <div class="border-t border-border px-5 py-3 flex justify-end gap-2">
      <button
        class="text-[13px] font-medium py-1.5 px-3 rounded-md border border-border hover:bg-muted transition-colors"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        class="text-[13px] font-medium py-1.5 px-3 rounded-md transition-all disabled:opacity-40"
        :class="destructive
          ? 'bg-destructive text-destructive-foreground hover:opacity-90'
          : 'bg-primary text-primary-foreground hover:opacity-90'"
        :disabled="loading"
        @click="emit('confirm')"
      >
        {{ loading ? 'Deleting...' : (confirmLabel ?? 'Confirm') }}
      </button>
    </div>
  </AppDialog>
</template>
