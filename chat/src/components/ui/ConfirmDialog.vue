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
    <div class="border-b border-foreground p-6 flex items-center justify-between">
      <h2 class="text-xl font-mono font-bold uppercase tracking-wider">{{ title }}</h2>
      <button class="p-1 hover:bg-muted transition-colors" @click="open = false">
        <X class="w-5 h-5" />
      </button>
    </div>

    <div class="p-6">
      <p class="font-mono text-sm leading-relaxed">{{ message }}</p>
    </div>

    <div class="border-t border-foreground p-6 flex justify-end gap-3">
      <button
        class="font-mono uppercase tracking-wider text-sm py-2 px-4 border border-foreground hover:bg-muted transition-colors"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        class="font-mono uppercase tracking-wider text-sm py-2 px-4 border transition-colors disabled:opacity-50"
        :class="destructive
          ? 'bg-destructive text-destructive-foreground border-destructive hover:bg-destructive/90'
          : 'bg-foreground text-background border-foreground hover:bg-foreground/90'"
        :disabled="loading"
        @click="emit('confirm')"
      >
        {{ loading ? 'Deleting...' : (confirmLabel ?? 'Confirm') }}
      </button>
    </div>
  </AppDialog>
</template>
