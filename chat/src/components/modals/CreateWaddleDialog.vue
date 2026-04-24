<script setup lang="ts">
import { Lock, Globe, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { CommunityFormData } from "@/lib/chat-ui";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  form: CommunityFormData;
  isSubmitting: boolean;
}>();

const emit = defineEmits<{
  submit: [];
  "update:form": [form: CommunityFormData];
}>();
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title">Create waddle</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        aria-label="Close create waddle dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="chat-dialog-stack chat-dialog-body">
      <div class="chat-field-stack">
        <label for="create-waddle-name" class="type-section-label text-muted-foreground">
          Name
        </label>
        <input
          id="create-waddle-name"
          :value="form.name"
          class="chat-field-control type-field"
          placeholder="My Waddle"
          @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div class="chat-field-stack">
        <label for="create-waddle-description" class="type-section-label text-muted-foreground">
          Description
        </label>
        <textarea
          id="create-waddle-description"
          :value="form.description"
          class="chat-field-control chat-textarea-control type-field"
          placeholder="What is this waddle about?"
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

    <div class="chat-dialog-footer">
      <button
        class="chat-action-button chat-action-button--secondary type-control"
        type="button"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        class="chat-action-button chat-action-button--primary type-control disabled:opacity-40"
        type="button"
        :disabled="isSubmitting || !form.name.trim()"
        @click="emit('submit')"
      >
        {{ isSubmitting ? "Creating…" : "Create" }}
      </button>
    </div>
  </AppDialog>
</template>
