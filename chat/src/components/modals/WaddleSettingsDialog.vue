<script setup lang="ts">
import { Lock, Globe, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { CommunityFormData } from "@/lib/chat-ui";
import type { WaddleSummary } from "@/lib/waddle-api";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  waddle: WaddleSummary | null;
  form: CommunityFormData;
  isSubmitting: boolean;
}>();

const emit = defineEmits<{
  save: [];
  delete: [];
  "update:form": [form: CommunityFormData];
}>();
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title">Waddle settings</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        aria-label="Close waddle settings dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="chat-dialog-stack chat-dialog-body">
      <div class="chat-field-stack">
        <label for="waddle-settings-name" class="type-section-label text-muted-foreground">
          Waddle name
        </label>
        <input
          id="waddle-settings-name"
          :value="form.name"
          class="chat-field-control type-field"
          @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div class="chat-field-stack">
        <label for="waddle-settings-description" class="type-section-label text-muted-foreground">
          Description
        </label>
        <textarea
          id="waddle-settings-description"
          :value="form.description"
          class="chat-field-control chat-textarea-control type-field"
          @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
        />
      </div>

      <div class="chat-field-stack">
        <label class="type-section-label text-muted-foreground">
          Privacy
        </label>
        <div class="flex gap-1.5">
          <button
            type="button"
            class="type-control chat-action-button flex-1 border"
            :class="!form.is_public ? 'border-primary bg-primary/5 text-foreground' : 'border-border hover:bg-muted'"
            :aria-pressed="!form.is_public"
            @click="$emit('update:form', { ...form, is_public: false })"
          >
            <Lock class="w-3.5 h-3.5" />
            Private
          </button>
          <button
            type="button"
            class="type-control chat-action-button flex-1 border"
            :class="form.is_public ? 'border-primary bg-primary/5 text-foreground' : 'border-border hover:bg-muted'"
            :aria-pressed="form.is_public"
            @click="$emit('update:form', { ...form, is_public: true })"
          >
            <Globe class="w-3.5 h-3.5" />
            Public
          </button>
        </div>
      </div>
    </div>

    <div class="chat-dialog-footer flex-col sm:flex-row sm:items-center sm:justify-between">
      <button
        class="chat-action-button type-control text-left text-destructive hover:bg-destructive/10 sm:text-center"
        type="button"
        @click="emit('delete')"
      >
        Delete waddle
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
          {{ isSubmitting ? "Saving…" : "Save changes" }}
        </button>
      </div>
    </div>
  </AppDialog>
</template>
