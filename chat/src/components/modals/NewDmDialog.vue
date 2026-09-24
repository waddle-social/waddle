<script setup lang="ts">
import { ref, watch } from "vue";
import { Loader2, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { UserSearchResult } from "@/lib/chat-types";

const open = defineModel<boolean>("open", { required: true });
const props = defineProps<{
  searchRecipients: (query: string) => Promise<UserSearchResult[]>;
}>();
const emit = defineEmits<{
  submit: [peerJid: string];
}>();

const query = ref("");
const results = ref<UserSearchResult[]>([]);
const selectedJid = ref<string | null>(null);
const isSearching = ref(false);
const searchError = ref("");

watch([open, query], ([isOpen, input], _previous, onCleanup) => {
  results.value = [];
  selectedJid.value = null;
  searchError.value = "";
  isSearching.value = false;
  if (!isOpen) {
    query.value = "";
    return;
  }
  if (!input.trim()) return;
  let cancelled = false;
  isSearching.value = true;
  const timer = setTimeout(async () => {
    try {
      const users = await props.searchRecipients(input);
      if (!cancelled) results.value = users;
    } catch (error) {
      if (!cancelled) searchError.value = error instanceof Error ? error.message : "Account search failed. Try again.";
    } finally {
      if (!cancelled) isSearching.value = false;
    }
  }, 220);
  onCleanup(() => {
    cancelled = true;
    clearTimeout(timer);
  });
}, { flush: "sync" });

function handleSubmit() {
  const recipient = results.value.find((user) => user.jid === selectedJid.value);
  if (!recipient || isSearching.value) return;
  emit("submit", recipient.jid);
  open.value = false;
}
</script>

<template>
  <AppDialog v-model:open="open" labelled-by="new-dm-title">
    <div class="chat-dialog-header">
      <h2 id="new-dm-title" class="type-dialog-title">New message</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        aria-label="Close new message dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <form id="new-dm-form" class="chat-field-stack chat-dialog-body" @submit.prevent="handleSubmit">
      <label for="new-dm-recipient" class="type-section-label text-muted-foreground">
        Username or account address
      </label>
      <input
        id="new-dm-recipient"
        v-model="query"
        class="chat-field-control type-field"
        placeholder="Search for an account"
        autocomplete="off"
        autofocus
      />
      <div v-if="results.length" class="flex max-h-64 flex-col gap-1 overflow-auto" aria-label="Account search results">
        <button
          v-for="user in results"
          :key="user.jid"
          class="chat-list-row flex min-h-12 w-full items-center gap-2.5 border p-2 text-left hover:bg-muted"
          :class="selectedJid === user.jid ? 'border-primary bg-primary/8' : 'border-border bg-muted/40'"
          type="button"
          :aria-pressed="selectedJid === user.jid"
          @click="selectedJid = user.jid"
        >
          <AppAvatar :name="user.display_name || user.username" :src="user.avatar_url" size="sm" />
          <span class="min-w-0 flex-1">
            <span class="type-control block truncate">{{ user.display_name || user.username }}</span>
            <span class="type-caption block truncate text-muted-foreground">{{ user.jid }}</span>
          </span>
        </button>
      </div>
      <div class="type-caption text-muted-foreground" role="status">
        <span v-if="isSearching" class="flex items-center gap-1.5">
          <Loader2 class="h-3.5 w-3.5 motion-safe:animate-spin" aria-hidden="true" />
          Searching…
        </span>
        <span v-else-if="searchError">{{ searchError }}</span>
        <span v-else-if="query.trim() && !results.length">No accounts found in the directory.</span>
        <span v-else-if="results.length">Select the account you want to message.</span>
      </div>
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
        type="submit"
        form="new-dm-form"
        :disabled="!selectedJid || isSearching"
      >
        Start conversation
      </button>
    </div>
  </AppDialog>
</template>
