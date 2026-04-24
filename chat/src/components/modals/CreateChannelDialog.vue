<script setup lang="ts">
import { Hash, MessagesSquare, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { ChannelCreateFormData } from "@/lib/chat-ui";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  form: ChannelCreateFormData;
  isSubmitting: boolean;
}>();

const emit = defineEmits<{
  submit: [];
  "update:form": [form: ChannelCreateFormData];
}>();
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title">Create MUC</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        aria-label="Close create MUC dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="chat-dialog-stack chat-dialog-body">
      <div class="chat-field-stack">
        <label for="create-channel-name" class="type-section-label text-muted-foreground">
          Name
        </label>
        <input
          id="create-channel-name"
          :value="form.name"
          class="chat-field-control type-field"
          placeholder="general"
          @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
        />
      </div>

      <div class="chat-field-stack">
        <label for="create-channel-description" class="type-section-label text-muted-foreground">
          Description
        </label>
        <textarea
          id="create-channel-description"
          :value="form.description"
          class="chat-field-control chat-textarea-control type-field"
          placeholder="What is this channel for?"
          @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
        />
      </div>

      <div class="chat-field-stack">
        <label class="type-section-label text-muted-foreground">
          MUC type
        </label>
        <div class="grid grid-cols-2 gap-2">
          <button
            type="button"
            class="flex flex-col gap-1 rounded-lg border px-3 py-3 text-left transition-all duration-200"
            :aria-pressed="form.channel_type === 'text'"
            :class="form.channel_type === 'text'
              ? 'border-primary/30 bg-primary/8 shadow-[0_0_12px_var(--glow)]'
              : 'border-border bg-muted/40 hover:bg-muted'"
            @click="$emit('update:form', { ...form, channel_type: 'text' })"
          >
            <div class="type-control flex items-center gap-2">
              <Hash class="w-4 h-4 text-primary/80" />
              Text
            </div>
            <p class="type-caption text-muted-foreground">
              Fast back-and-forth conversation.
            </p>
          </button>
          <button
            type="button"
            class="flex flex-col gap-1 rounded-lg border px-3 py-3 text-left transition-all duration-200"
            :aria-pressed="form.channel_type === 'forum'"
            :class="form.channel_type === 'forum'
              ? 'border-primary/30 bg-primary/8 shadow-[0_0_12px_var(--glow)]'
              : 'border-border bg-muted/40 hover:bg-muted'"
            @click="$emit('update:form', { ...form, channel_type: 'forum' })"
          >
            <div class="type-control flex items-center gap-2">
              <MessagesSquare class="w-4 h-4 text-primary/80" />
              Forum
            </div>
            <p class="type-caption text-muted-foreground">
              Title-first topics with threaded replies.
            </p>
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
