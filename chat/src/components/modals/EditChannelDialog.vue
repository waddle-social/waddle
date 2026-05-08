<script setup lang="ts">
import { X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { ChannelEditFormData } from "@/lib/chat-ui";
import type { ChannelSummary, PinPermission } from "@/lib/chat-types";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  channel: ChannelSummary | null;
  form: ChannelEditFormData;
  isSubmitting: boolean;
}>();

const emit = defineEmits<{
  save: [];
  delete: [];
  "update:form": [form: ChannelEditFormData];
}>();
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title truncate">
        {{ channel ? `Edit #${channel.name}` : "Edit channel" }}
      </h2>
      <button
        class="chat-icon-button flex-none hover:bg-muted"
        type="button"
        aria-label="Close edit channel dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="chat-dialog-stack chat-dialog-body">
      <div class="chat-field-stack">
        <label for="edit-channel-name" class="type-section-label text-muted-foreground">
          Name
        </label>
        <input
          id="edit-channel-name"
          :value="form.name"
          class="chat-field-control type-field"
          @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div class="chat-field-stack">
        <label for="edit-channel-description" class="type-section-label text-muted-foreground">
          Description
        </label>
        <textarea
          id="edit-channel-description"
          :value="form.description"
          class="chat-field-control chat-textarea-control type-field"
          @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
        />
      </div>

      <div class="chat-field-stack">
        <label for="edit-channel-pin-permission" class="type-section-label text-muted-foreground">
          Who can pin messages
        </label>
        <select
          id="edit-channel-pin-permission"
          :value="form.pinPermission"
          class="chat-field-control type-field"
          @change="$emit('update:form', { ...form, pinPermission: ($event.target as HTMLSelectElement).value as PinPermission })"
        >
          <option value="admins-only">Admins only</option>
          <option value="anyone">Anyone</option>
        </select>
      </div>
    </div>

    <div class="chat-dialog-footer flex-col sm:flex-row sm:items-center sm:justify-between">
      <button
        class="chat-action-button type-control text-left text-destructive hover:bg-destructive/10 sm:text-center"
        type="button"
        @click="emit('delete')"
      >
        Delete channel
      </button>
      <div class="flex justify-end gap-2">
        <button
          class="chat-action-button chat-action-button--secondary type-control flex-1 sm:flex-none"
          type="button"
          @click="open = false"
        >
          Cancel
        </button>
        <button
          class="chat-action-button chat-action-button--primary type-control flex-1 disabled:opacity-40 sm:flex-none"
          type="button"
          :disabled="isSubmitting || !form.name.trim()"
          @click="emit('save')"
        >
          {{ isSubmitting ? "Saving…" : "Save" }}
        </button>
      </div>
    </div>
  </AppDialog>
</template>
