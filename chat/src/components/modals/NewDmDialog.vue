<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { X } from "lucide-vue-next";
import { css } from "styled-system/css";
import { btn, chip, iconbtn, waddle } from "styled-system/recipes";
import AppDialog from "@/components/ui/AppDialog.vue";
import { dialogParts } from "@/ui/part-classes";
import { formClasses as f } from "@/components/modals/form-classes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import type { UserSearchResult } from "@/lib/chat-types";
import type { RosterContact } from "@/lib/xmpp/types";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp/jid";
import { isBotJid } from "@/lib/avatars/author-jid";

/**
 * The one way to start a conversation. Pick one person for a direct message,
 * or two or more for a group. Opened from a 1:1 DM ("Add people") it carries
 * that person as `seed`, so one more pick makes a group.
 *
 * People only come from directory (XEP-0055) results or your contacts; typed
 * text is never turned into an address.
 */
interface Person {
  jid: string;
  label: string;
}

const open = defineModel<boolean>("open", { required: true });
const props = defineProps<{
  searchRecipients: (query: string) => Promise<UserSearchResult[]>;
  contacts?: RosterContact[];
  selfJid?: string | null;
  seed?: Person | null;
  isSubmitting?: boolean;
}>();
const emit = defineEmits<{
  submit: [peerJid: string];
  createGroup: [payload: { name: string; memberJids: string[] }];
}>();

const query = ref("");
const results = ref<UserSearchResult[]>([]);
const picked = ref<Person[]>([]);
const name = ref("");
const isSearching = ref(false);
const searchError = ref("");

watch([open, query], ([isOpen, input], _previous, onCleanup) => {
  results.value = [];
  searchError.value = "";
  isSearching.value = false;
  if (!isOpen) {
    query.value = "";
    picked.value = [];
    name.value = "";
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

/** Contacts before you type, directory results once you do. */
const candidates = computed<Person[]>(() => {
  const skip = new Set([props.selfJid, props.seed?.jid].map((jid) => barePeerJid(jid ?? "").toLowerCase()));
  const people = query.value.trim()
    ? results.value.map((user) => ({ jid: user.jid, label: user.display_name || user.username }))
    : (props.contacts ?? []).map((contact) => ({
        jid: contact.jid,
        label: contact.name?.trim() || contact.username?.trim() || jidLocalpart(contact.jid) || contact.jid,
      }));
  return people.filter((person) => {
    const bare = barePeerJid(person.jid).toLowerCase();
    return !skip.has(bare) && !isBotJid(bare);
  });
});

const isGroup = computed(() => picked.value.length + (props.seed ? 1 : 0) >= 2);
const canSubmit = computed(() => picked.value.length > 0 && !props.isSubmitting);
const defaultName = computed(() =>
  [props.seed, ...picked.value].filter(Boolean).map((person) => person!.label).join(", "),
);

function isPicked(jid: string): boolean {
  return picked.value.some((person) => person.jid === jid);
}

function toggle(person: Person) {
  picked.value = isPicked(person.jid)
    ? picked.value.filter((p) => p.jid !== person.jid)
    : [...picked.value, person];
}

// Enter while searching picks the top match and clears the box for the next
// name, instead of submitting whoever was picked before.
function onSearchEnter(event: KeyboardEvent) {
  if (!query.value.trim()) return;
  event.preventDefault();
  const first = candidates.value[0];
  if (!first || isSearching.value) return;
  if (!isPicked(first.jid)) toggle(first);
  query.value = "";
}

function handleSubmit() {
  if (!canSubmit.value) return;
  if (isGroup.value) {
    emit("createGroup", {
      name: name.value.trim() || defaultName.value,
      memberJids: picked.value.map((person) => person.jid),
    });
    return;
  }
  emit("submit", picked.value[0]!.jid);
  open.value = false;
}

const list = css({ display: "flex", flexDirection: "column", gap: "0.5", maxHeight: "16rem", overflowY: "auto", mx: "-2" });
const chips = css({ display: "flex", flexWrap: "wrap", gap: "1.5" });
const chipRemove = css({
  display: "inline-flex",
  border: 0,
  p: 0,
  ml: "1",
  bg: "transparent",
  color: "inherit",
  cursor: "pointer",
  "& svg": { width: "12px", height: "12px" },
});
</script>

<template>
  <AppDialog v-model:open="open" labelled-by="new-dm-title">
    <div :class="dialogParts.head">
      <h2 id="new-dm-title">{{ seed ? "Add people" : "New message" }}</h2>
      <button :class="iconbtn()" type="button" aria-label="Close new message dialog" @click="open = false">
        <X aria-hidden="true" />
      </button>
    </div>

    <form id="new-dm-form" :class="dialogParts.body" @submit.prevent="handleSubmit">
      <div v-if="seed || picked.length" :class="chips" role="list" aria-label="People in this conversation">
        <span v-if="seed" :class="chip()" role="listitem">{{ seed.label }}</span>
        <span v-for="person in picked" :key="person.jid" :class="chip()" role="listitem">
          {{ person.label }}
          <button :class="chipRemove" type="button" :aria-label="`Remove ${person.label}`" @click="toggle(person)">
            <X aria-hidden="true" />
          </button>
        </span>
      </div>
      <div :class="f.field">
        <label for="new-dm-recipient" :class="f.label">Add people</label>
        <input
          id="new-dm-recipient"
          v-model="query"
          :class="f.input"
          placeholder="Search by name or account address"
          autocomplete="off"
          autofocus
          @keydown.enter="onSearchEnter"
        />
      </div>
      <div v-if="candidates.length" :class="list" :aria-label="query.trim() ? 'Account search results' : 'Your contacts'">
        <button
          v-for="person in candidates"
          :key="person.jid"
          :class="f.pickRow"
          type="button"
          :aria-pressed="isPicked(person.jid)"
          @click="toggle(person)"
        >
          <UserAvatar :name="person.label" :jid="person.jid" size="sm" />
          <span :class="f.rowText">
            <span :class="f.rowName">{{ person.label }}</span>
            <span :class="f.rowSub">{{ person.jid }}</span>
          </span>
        </button>
      </div>
      <div v-if="isGroup" :class="f.field">
        <label for="new-dm-name" :class="f.label">Group name</label>
        <input id="new-dm-name" v-model="name" :class="f.input" :placeholder="defaultName" />
      </div>
      <div :class="f.status" role="status">
        <template v-if="isSearching">
          <span :class="waddle()" aria-hidden="true"><i /><i /><i /></span>
          Searching…
        </template>
        <span v-else-if="searchError">{{ searchError }}</span>
        <span v-else-if="query.trim() && !results.length">No accounts found in the directory.</span>
        <span v-else-if="!picked.length">{{ seed ? "Pick who else to bring in." : "Pick one person for a direct message, or more for a group." }}</span>
      </div>
    </form>

    <div :class="dialogParts.foot">
      <button :class="btn()" type="button" @click="open = false">Cancel</button>
      <button :class="btn({ variant: 'primary' })" type="submit" form="new-dm-form" :disabled="!canSubmit">
        {{ isGroup || seed ? "Start group" : "Start conversation" }}
      </button>
    </div>
  </AppDialog>
</template>
