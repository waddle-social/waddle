<script setup lang="ts">
// Admin V2 — modal for `adminChannelsCreate`. `is_public` defaults to
// `true` per the V2 spec; the helper text reflects that.
import { ref, watch } from "vue";
import { X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  isSubmitting?: boolean;
  spaces?: { space_jid: string; name: string }[];
}>();

const emit = defineEmits<{
  submit: [payload: { name: string; topic?: string | null; spaceJid?: string | null; isPublic?: boolean | null }];
}>();

const name = ref("");
const topic = ref("");
const spaceJid = ref("");
const isPublic = ref(true);

watch(open, (isOpen) => {
  if (isOpen) {
    name.value = "";
    topic.value = "";
    spaceJid.value = "";
    isPublic.value = true;
  }
});

function onSubmit() {
  if (!name.value.trim() || props.isSubmitting) return;
  emit("submit", {
    name: name.value.trim(),
    topic: topic.value.trim() || null,
    spaceJid: spaceJid.value || null,
    isPublic: isPublic.value,
  });
}
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title">New Channel</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        aria-label="Close create-channel dialog"
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
          placeholder="general"
        />
      </label>
      <label class="flex flex-col gap-1">
        <span class="type-section-label text-muted-foreground">Topic (optional)</span>
        <textarea
          v-model="topic"
          rows="2"
          class="chat-field-control chat-textarea-control type-field"
          placeholder="What's this channel for?"
        />
      </label>
      <label v-if="spaces && spaces.length > 0" class="flex flex-col gap-1">
        <span class="type-section-label text-muted-foreground">Space (optional)</span>
        <select v-model="spaceJid" class="chat-field-control type-field">
          <option value="">No space</option>
          <option v-for="s in spaces" :key="s.space_jid" :value="s.space_jid">{{ s.name }}</option>
        </select>
      </label>
      <div class="flex flex-col gap-1.5 rounded-lg border border-border bg-muted/30 px-3 py-2.5">
        <label class="flex items-center gap-2 cursor-pointer">
          <input v-model="isPublic" type="checkbox" />
          <span class="type-control">Public</span>
        </label>
        <p class="type-caption text-muted-foreground">
          Anyone in the community can join. Uncheck to require explicit
          membership.
        </p>
      </div>
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
        {{ isSubmitting ? "Creating…" : "Create channel" }}
      </button>
    </div>
  </AppDialog>
</template>
