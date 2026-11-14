<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Check, X } from "lucide-vue-next";
import { css } from "styled-system/css";
import { btn, iconbtn } from "styled-system/recipes";
import AppDialog from "@/components/ui/AppDialog.vue";
import { dialogParts } from "@/ui/part-classes";
import { formClasses as f } from "@/components/modals/form-classes";
import type { RosterContact } from "@/lib/xmpp/types";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp/jid";
import { isBotJid } from "@/lib/avatars/author-jid";

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
  const self = barePeerJid(props.selfJid ?? "").toLowerCase();
  const excluded = new Set((props.excludedJids ?? []).map((jid) => barePeerJid(jid).toLowerCase()));
  return props.contacts
    .filter((contact) => {
      const bare = barePeerJid(contact.jid).toLowerCase();
      return bare !== self && !excluded.has(bare) && !isBotJid(bare);
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
  return contact.name?.trim() || contact.username?.trim() || jidLocalpart(contact.jid) || contact.jid;
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
    name: name.value.trim(),
    memberJids: [...selected.value],
  });
}
const list = css({ display: "grid", gap: "0.5", maxHeight: "18rem", overflowY: "auto", mx: "-2" });
const check = css({
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  width: "18px",
  height: "18px",
  flex: "none",
  borderRadius: "4px",
  border: "1px solid {colors.border.strong}",
  color: "action.fg",
  "[aria-pressed=true] > &": { bg: "action", borderColor: "action" },
  "& svg": { width: "12px", height: "12px", strokeWidth: 3 },
});
</script>

<template>
  <AppDialog v-model:open="open" labelled-by="new-group-dm-title">
    <div :class="dialogParts.head">
      <h2 id="new-group-dm-title">{{ props.title ?? "New group message" }}</h2>
      <button
        :class="iconbtn()"
        type="button"
        :aria-label="`Close ${props.title ?? 'new group message'} dialog`"
        @click="open = false"
      >
        <X aria-hidden="true" />
      </button>
    </div>

    <div :class="dialogParts.body">
      <div :class="f.field">
        <label for="new-group-dm-name" :class="f.label">Name</label>
        <input
          id="new-group-dm-name"
          v-model="name"
          :class="f.input"
          :placeholder="defaultName || 'Alice, Bob'"
        />
      </div>

      <div :class="list">
        <button
          v-for="contact in selectableContacts"
          :key="contact.jid"
          :class="f.pickRow"
          type="button"
          :aria-pressed="selected.has(contact.jid)"
          @click="toggleContact(contact.jid)"
        >
          <span :class="check" aria-hidden="true">
            <Check v-if="selected.has(contact.jid)" />
          </span>
          <span :class="f.rowText">
            <span :class="f.rowName">{{ contactLabel(contact) }}</span>
            <span :class="f.rowSub">{{ contact.jid }}</span>
          </span>
        </button>
      </div>
    </div>

    <div :class="dialogParts.foot">
      <button :class="btn()" type="button" @click="open = false">Cancel</button>
      <button :class="btn({ variant: 'primary' })" type="button" :disabled="!canSubmit" @click="handleSubmit">
        {{ (props.minimumSelectedContacts ?? 2) < 2 ? "Add people" : "Create group" }}
      </button>
    </div>
  </AppDialog>
</template>
