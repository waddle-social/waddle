<script setup lang="ts">
// Admin V2 — modal for `adminSpacesCreate`. Mirrors the dialog
// surface used elsewhere in chat (AppDialog + `chat-dialog-*` classes)
// so the surface feels native to the rest of the admin shell.
import { ref, watch } from "vue";
import { X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  isSubmitting?: boolean;
}>();

const emit = defineEmits<{
  /** Caller invokes `adminSpacesCreate` with these args and resolves. */
  submit: [payload: { name: string; description?: string | null; iconUrl?: string | null }];
}>();

const name = ref("");
const description = ref("");
const iconUrl = ref("");

watch(open, (isOpen) => {
  if (isOpen) {
    name.value = "";
    description.value = "";
    iconUrl.value = "";
  }
});

function onSubmit() {
  if (!name.value.trim() || props.isSubmitting) return;
  emit("submit", {
    name: name.value.trim(),
    description: description.value.trim() || null,
    iconUrl: iconUrl.value.trim() || null,
  });
}
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title">New Space</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        aria-label="Close create-space dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <form class="chat-dialog-body flex flex-col gap-3" @submit.prevent="onSubmit">
      <label class="flex flex-col gap-1">
        <span class="type-section-label text-muted-foreground">Name</span>
        <input
          v-model="name"
          type="text"
          required
          maxlength="80"
          autocomplete="off"
          class="chat-field-control type-field"
          placeholder="Engineering"
        />
      </label>
      <label class="flex flex-col gap-1">
        <span class="type-section-label text-muted-foreground">Description (optional)</span>
        <textarea
          v-model="description"
          rows="3"
          class="chat-field-control chat-textarea-control type-field"
          placeholder="What's this space for?"
        />
      </label>
      <label class="flex flex-col gap-1">
        <span class="type-section-label text-muted-foreground">Icon URL (optional)</span>
        <input
          v-model="iconUrl"
          type="url"
          autocomplete="off"
          class="chat-field-control type-field"
          placeholder="https://…"
        />
      </label>
    </form>

    <div class="chat-dialog-footer">
      <button
        type="button"
        class="chat-action-button chat-action-button--secondary type-control"
        :disabled="isSubmitting"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        type="button"
        class="chat-action-button chat-action-button--primary type-action disabled:opacity-30"
        :disabled="!name.trim() || isSubmitting"
        @click="onSubmit"
      >
        {{ isSubmitting ? "Creating…" : "Create space" }}
      </button>
    </div>
  </AppDialog>
</template>
