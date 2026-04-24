<script setup lang="ts">
import { ref } from "vue";
import { X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";

const open = defineModel<boolean>("open", { required: true });

const emit = defineEmits<{
  submit: [username: string];
}>();

const username = ref("");

function handleSubmit() {
  const trimmed = username.value.replace(/^@/, "").trim();
  if (!trimmed) return;
  emit("submit", trimmed);
  username.value = "";
  open.value = false;
}
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title">New message</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        aria-label="Close new message dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <form class="chat-field-stack chat-dialog-body" @submit.prevent="handleSubmit">
      <label for="new-dm-username" class="type-section-label text-muted-foreground">
        Username
      </label>
      <input
        id="new-dm-username"
        v-model="username"
        class="chat-field-control type-field"
        placeholder="alice"
        autofocus
      />
    </form>

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
        :disabled="!username.replace(/^@/, '').trim()"
        @click="handleSubmit"
      >
        Start conversation
      </button>
    </div>
  </AppDialog>
</template>
