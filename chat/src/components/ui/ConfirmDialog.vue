<script setup lang="ts">
import { Dialog } from "@ark-ui/vue/dialog";
import { X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";

/**
 * Confirm / destructive action prompt. An `alertdialog` on `AppDialog`:
 * Ark links the title and message as the dialog's accessible name and
 * description.
 */
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
  <AppDialog v-model:open="open" role="alertdialog">
    <div class="chat-dialog-header">
      <Dialog.Title as-child>
        <h2 class="type-dialog-title">{{ title }}</h2>
      </Dialog.Title>
      <Dialog.CloseTrigger as-child>
        <button
          class="chat-icon-button hover:bg-muted"
          type="button"
          aria-label="Close confirmation dialog"
        >
          <X class="w-4 h-4 text-muted-foreground" />
        </button>
      </Dialog.CloseTrigger>
    </div>

    <div class="chat-dialog-body">
      <Dialog.Description as-child>
        <p class="type-field text-muted-foreground">{{ message }}</p>
      </Dialog.Description>
    </div>

    <div class="chat-dialog-footer">
      <Dialog.CloseTrigger as-child>
        <button class="chat-action-button chat-action-button--secondary type-control" type="button">
          Cancel
        </button>
      </Dialog.CloseTrigger>
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
