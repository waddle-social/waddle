<script setup lang="ts">
import { ref, watch } from "vue";
import { X } from "lucide-vue-next";
import { css } from "styled-system/css";
import { btn, iconbtn, waddle } from "styled-system/recipes";
import AppDialog from "@/components/ui/AppDialog.vue";
import { dialogParts } from "@/ui/part-classes";
import { formClasses as f } from "@/components/modals/form-classes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
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
const results_ = css({ display: "flex", flexDirection: "column", gap: "0.5", maxHeight: "16rem", overflowY: "auto", mx: "-2" });
</script>

<template>
  <AppDialog v-model:open="open" labelled-by="new-dm-title">
    <div :class="dialogParts.head">
      <h2 id="new-dm-title">New message</h2>
      <button :class="iconbtn()" type="button" aria-label="Close new message dialog" @click="open = false">
        <X aria-hidden="true" />
      </button>
    </div>

    <form id="new-dm-form" :class="dialogParts.body" @submit.prevent="handleSubmit">
      <div :class="f.field">
        <label for="new-dm-recipient" :class="f.label">Username or account address</label>
        <input
          id="new-dm-recipient"
          v-model="query"
          :class="f.input"
          placeholder="Search for an account"
          autocomplete="off"
          autofocus
        />
      </div>
      <div v-if="results.length" :class="results_" aria-label="Account search results">
        <button
          v-for="user in results"
          :key="user.jid"
          :class="f.pickRow"
          type="button"
          :aria-pressed="selectedJid === user.jid"
          @click="selectedJid = user.jid"
        >
          <UserAvatar :name="user.display_name || user.username" :jid="user.jid" size="sm" />
          <span :class="f.rowText">
            <span :class="f.rowName">{{ user.display_name || user.username }}</span>
            <span :class="f.rowSub">{{ user.jid }}</span>
          </span>
        </button>
      </div>
      <div :class="f.status" role="status">
        <template v-if="isSearching">
          <span :class="waddle()" aria-hidden="true"><i /><i /><i /></span>
          Searching…
        </template>
        <span v-else-if="searchError">{{ searchError }}</span>
        <span v-else-if="query.trim() && !results.length">No accounts found in the directory.</span>
        <span v-else-if="results.length">Select the account you want to message.</span>
      </div>
    </form>

    <div :class="dialogParts.foot">
      <button :class="btn()" type="button" @click="open = false">Cancel</button>
      <button
        :class="btn({ variant: 'primary' })"
        type="submit"
        form="new-dm-form"
        :disabled="!selectedJid || isSearching"
      >
        Start conversation
      </button>
    </div>
  </AppDialog>
</template>
