<script setup lang="ts">
// Admin V2 — modal for `adminChannelsCreate`.
import { ref, watch } from "vue";
import { X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import { requireMembershipForUnlistedChannel } from "./channelAdmissionPolicy";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  isSubmitting?: boolean;
  spaces?: { space_jid: string; space_node: string; name: string }[];
}>();

const emit = defineEmits<{
  submit: [payload: { name: string; topic?: string | null; spaceJid?: string | null; spaceNode?: string | null; isPublic?: boolean | null; membersOnly?: boolean | null }];
}>();

const name = ref("");
const topic = ref("");
const spaceNode = ref("");
const isPublic = ref(true);
const membersOnly = ref(false);

watch(open, (isOpen) => {
  if (isOpen) {
    name.value = "";
    topic.value = "";
    spaceNode.value = "";
    isPublic.value = true;
    membersOnly.value = false;
  }
});

function handlePublicToggleChange() {
  const policy = requireMembershipForUnlistedChannel({
    isPublic: isPublic.value,
    membersOnly: membersOnly.value,
  });
  isPublic.value = policy.isPublic;
  membersOnly.value = policy.membersOnly;
}

function onSubmit() {
  if (!name.value.trim() || props.isSubmitting) return;
  const selectedSpace = props.spaces?.find((space) => space.space_node === spaceNode.value);
  emit("submit", {
    name: name.value.trim(),
    topic: topic.value.trim() || null,
    spaceJid: selectedSpace?.space_jid ?? null,
    spaceNode: selectedSpace?.space_node ?? null,
    isPublic: isPublic.value,
    membersOnly: membersOnly.value,
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
        <select v-model="spaceNode" class="chat-field-control type-field">
          <option value="">No space</option>
          <option v-for="s in spaces" :key="s.space_node" :value="s.space_node">{{ s.name }}</option>
        </select>
      </label>
      <div class="flex flex-col gap-2 rounded-lg border border-border bg-muted/30 px-3 py-2.5">
        <label class="flex items-center gap-2 cursor-pointer">
          <input v-model="isPublic" type="checkbox" @change="handlePublicToggleChange" />
          <span class="type-control">Listed in discovery</span>
        </label>
        <label class="flex items-center gap-2 cursor-pointer">
          <input v-model="membersOnly" type="checkbox" />
          <span class="type-control">Require explicit membership</span>
        </label>
        <p class="type-caption text-muted-foreground">
          Discovery visibility and room admission are separate XMPP room
          settings.
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
