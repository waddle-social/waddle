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
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title">{{ title }}</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        aria-label="Close confirmation dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="chat-dialog-body">
      <p class="type-field text-muted-foreground">{{ message }}</p>
    </div>

    <div class="chat-dialog-footer">
      <button
        class="chat-action-button chat-action-button--secondary type-control"
        type="button"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        class="chat-action-button type-action disabled:opacity-30"
        type="button"
        :class="destructive
          ? 'chat-action-button--destructive'
          : 'chat-action-button--primary'"
        :disabled="loading"
        @click="emit('confirm')"
      >
        {{ loading ? "Deleting…" : (confirmLabel ?? "Confirm") }}
      </button>
    </div>
  </AppDialog>
</template>
