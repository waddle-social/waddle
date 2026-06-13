<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Check, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type { RosterContact } from "@/lib/xmpp/types";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  contacts: RosterContact[];
  isSubmitting?: boolean;
  selfJid?: string | null;
  excludedJids?: readonly string[];
  minimumSelectedContacts?: number;
  title?: string;
}>();

const emit = defineEmits<{
  submit: [payload: { name: string; memberJids: string[] }];
}>();

const selected = ref<Set<string>>(new Set());

const selectableContacts = computed(() => {
  const self = (props.selfJid ?? "").split("/")[0]?.toLowerCase() ?? "";
  const excluded = new Set((props.excludedJids ?? []).map((jid) => jid.split("/")[0]?.toLowerCase() ?? ""));
  return props.contacts
    .filter((contact) => {
      const bare = contact.jid.split("/")[0]?.toLowerCase() ?? "";
      return bare !== self && !excluded.has(bare);
    })
    .sort((a, b) => contactLabel(a).localeCompare(contactLabel(b), undefined, { sensitivity: "base" }));
});

const selectedContacts = computed(() =>
  selectableContacts.value.filter((contact) => selected.value.has(contact.jid)),
);

const defaultName = computed(() => selectedContacts.value.map(contactLabel).join(", "));
const name = ref("");
const canSubmit = computed(() => selected.value.size >= (props.minimumSelectedContacts ?? 2) && !props.isSubmitting);

watch(open, (next) => {
  if (!next) {
    selected.value = new Set();
    name.value = "";
  }
});

function contactLabel(contact: RosterContact): string {
  return contact.name?.trim() || contact.username?.trim() || contact.jid.split("@")[0] || contact.jid;
}

function toggleContact(jid: string) {
  const next = new Set(selected.value);
  if (next.has(jid)) next.delete(jid);
  else next.add(jid);
  selected.value = next;
}

function handleSubmit() {
  if (!canSubmit.value) return;
  emit("submit", {
    name: name.value.trim() || defaultName.value,
    memberJids: [...selected.value],
  });
}
</script>

<template>
  <AppDialog v-model:open="open" labelled-by="new-group-dm-title">
    <div class="chat-dialog-header">
      <h2 id="new-group-dm-title" class="type-dialog-title">{{ props.title ?? "New group message" }}</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        :aria-label="`Close ${props.title ?? 'new group message'} dialog`"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="chat-field-stack chat-dialog-body">
      <label for="new-group-dm-name" class="type-section-label text-muted-foreground">
        Name
      </label>
      <input
        id="new-group-dm-name"
        v-model="name"
        class="chat-field-control type-field"
        :placeholder="defaultName || 'Alice, Bob'"
      />

      <div class="grid max-h-72 gap-1 overflow-auto pr-1">
        <button
          v-for="contact in selectableContacts"
          :key="contact.jid"
          class="chat-list-row flex min-h-11 items-center gap-3 rounded-md px-3 py-2 text-left hover:bg-muted"
          type="button"
          :aria-pressed="selected.has(contact.jid)"
          @click="toggleContact(contact.jid)"
        >
          <span class="flex h-5 w-5 shrink-0 items-center justify-center rounded border border-border">
            <Check v-if="selected.has(contact.jid)" class="h-3.5 w-3.5 text-primary" />
          </span>
          <span class="min-w-0 flex-1">
            <span class="type-control block truncate text-foreground">{{ contactLabel(contact) }}</span>
            <span class="type-caption block truncate text-muted-foreground">{{ contact.jid }}</span>
          </span>
        </button>
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
        :disabled="!canSubmit"
        @click="handleSubmit"
      >
        {{ props.title === "Add people" ? "Add people" : "Create group" }}
      </button>
    </div>
  </AppDialog>
</template>
