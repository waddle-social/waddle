<script setup lang="ts">
// Admin V2 — slide-from-right drawer for editing a channel. Three
// tabs (Config / Affiliations / Occupants) + a danger section with a
// destroy-room confirm flow.
import { computed, onMounted, ref, watch } from "vue";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import AffiliationRow from "@/components/admin/AffiliationRow.vue";
import OccupantRow from "@/components/admin/OccupantRow.vue";
import type { BrowserXmppClient } from "@/lib/xmpp";
import type {
  WasmAdminChannelAffiliationEntry,
  WasmAdminChannelListEntry,
  WasmAdminChannelOccupantEntry,
} from "@/lib/xmpp";

type Affiliation = "owner" | "admin" | "member" | "none" | "outcast";
type Tab = "config" | "affiliations" | "occupants";

const props = defineProps<{
  open: boolean;
  xmppClient: BrowserXmppClient | null;
  channel: WasmAdminChannelListEntry;
}>();

const emit = defineEmits<{
  close: [];
  changed: [];
  deleted: [];
}>();

const openLocal = ref(props.open);
watch(() => props.open, (v) => { openLocal.value = v; });
watch(openLocal, (v) => { if (!v) emit("close"); });

const tab = ref<Tab>("config");

// Config edit state
const editName = ref(props.channel.name);
const editTopic = ref(props.channel.topic ?? "");
const editIsPublic = ref(props.channel.is_public);
const editing = ref(false);
const editError = ref("");

// Affiliations
const affiliations = ref<WasmAdminChannelAffiliationEntry[]>([]);
const affiliationsLoading = ref(false);
const affiliationsError = ref("");
const affiliationMutating = ref<string | null>(null);

// Occupants
const occupants = ref<WasmAdminChannelOccupantEntry[]>([]);
const occupantsLoading = ref(false);
const occupantsError = ref("");
const occupantKicking = ref<string | null>(null);

// Delete
const showDelete = ref(false);
const deleting = ref(false);
const deleteError = ref("");

watch(() => props.channel, (c) => {
  editName.value = c.name;
  editTopic.value = c.topic ?? "";
  editIsPublic.value = c.is_public;
  void loadAffiliations();
  void loadOccupants();
}, { immediate: false });

async function loadAffiliations() {
  if (!props.xmppClient) return;
  affiliationsLoading.value = true;
  affiliationsError.value = "";
  try {
    const page = await props.xmppClient.adminChannelsAffiliations({
      channelJid: props.channel.channel_jid,
      pageSize: 200,
    });
    affiliations.value = page.entries;
  } catch (err: unknown) {
    affiliationsError.value = err instanceof Error ? err.message : "Failed to load affiliations.";
  } finally {
    affiliationsLoading.value = false;
  }
}

async function loadOccupants() {
  if (!props.xmppClient) return;
  occupantsLoading.value = true;
  occupantsError.value = "";
  try {
    const page = await props.xmppClient.adminChannelsOccupants({
      channelJid: props.channel.channel_jid,
      pageSize: 200,
    });
    occupants.value = page.entries;
  } catch (err: unknown) {
    occupantsError.value = err instanceof Error ? err.message : "Failed to load occupants.";
  } finally {
    occupantsLoading.value = false;
  }
}

async function saveConfig() {
  if (!props.xmppClient) return;
  if (!editName.value.trim()) return;
  editing.value = true;
  editError.value = "";
  try {
    await props.xmppClient.adminChannelsUpdate({
      channelJid: props.channel.channel_jid,
      name: editName.value.trim(),
      topic: editTopic.value.trim() || null,
      isPublic: editIsPublic.value,
    });
    emit("changed");
  } catch (err: unknown) {
    editError.value = err instanceof Error ? err.message : "Failed to update channel.";
  } finally {
    editing.value = false;
  }
}

async function changeAffiliation(entry: WasmAdminChannelAffiliationEntry, next: Affiliation) {
  if (!props.xmppClient) return;
  affiliationMutating.value = entry.jid;
  try {
    await props.xmppClient.adminChannelsSetAffiliation({
      channelJid: props.channel.channel_jid,
      memberJid: entry.jid,
      affiliation: next,
    });
    await loadAffiliations();
    emit("changed");
  } catch (err: unknown) {
    affiliationsError.value = err instanceof Error ? err.message : "Failed to update affiliation.";
  } finally {
    affiliationMutating.value = null;
  }
}

async function kickOccupant(entry: WasmAdminChannelOccupantEntry) {
  if (!props.xmppClient) return;
  // Derive a bare JID from the full real_jid the server emits.
  const bareJid = entry.real_jid.split("/")[0] ?? entry.real_jid;
  occupantKicking.value = entry.real_jid;
  try {
    await props.xmppClient.adminChannelsKick({
      channelJid: props.channel.channel_jid,
      occupantJid: bareJid,
    });
    await loadOccupants();
    emit("changed");
  } catch (err: unknown) {
    occupantsError.value = err instanceof Error ? err.message : "Failed to kick occupant.";
  } finally {
    occupantKicking.value = null;
  }
}

async function confirmDelete() {
  if (!props.xmppClient) return;
  deleting.value = true;
  deleteError.value = "";
  try {
    await props.xmppClient.adminChannelsDelete({ channelJid: props.channel.channel_jid });
    showDelete.value = false;
    emit("deleted");
  } catch (err: unknown) {
    deleteError.value = err instanceof Error ? err.message : "Failed to delete channel.";
  } finally {
    deleting.value = false;
  }
}

onMounted(() => {
  void loadAffiliations();
  void loadOccupants();
});

const tabs: { value: Tab; label: string }[] = [
  { value: "config", label: "Config" },
  { value: "affiliations", label: "Affiliations" },
  { value: "occupants", label: "Occupants" },
];

const affiliationCountLabel = computed(() => {
  if (affiliations.value.length === 0) return "";
  return ` (${affiliations.value.length})`;
});
const occupantCountLabel = computed(() => {
  if (occupants.value.length === 0) return "";
  return ` (${occupants.value.length})`;
});
</script>

<template>
  <AppDrawer v-model:open="openLocal" side="right" label="Channel details" width-class="w-full max-w-md lg:max-w-lg">
    <template #title>
      <span class="type-pane-title truncate">{{ channel.name }}</span>
    </template>

    <div class="flex flex-col gap-4 p-4">
      <!-- Tabs -->
      <div class="flex items-center gap-1 border-b border-border" role="tablist">
        <button
          v-for="t in tabs"
          :key="t.value"
          type="button"
          role="tab"
          :aria-selected="tab === t.value"
          class="px-3 py-1.5 type-control border-b-2 -mb-px transition-colors"
          :class="tab === t.value ? 'border-primary text-primary' : 'border-transparent text-muted-foreground hover:text-foreground'"
          @click="tab = t.value"
        >
          {{ t.label }}<template v-if="t.value === 'affiliations'">{{ affiliationCountLabel }}</template>
          <template v-else-if="t.value === 'occupants'">{{ occupantCountLabel }}</template>
        </button>
      </div>

      <!-- Config tab -->
      <section v-if="tab === 'config'" class="flex flex-col gap-3">
        <label class="flex flex-col gap-1">
          <span class="type-section-label text-muted-foreground">Name</span>
          <input v-model="editName" type="text" maxlength="80" class="chat-field-control type-field" />
        </label>
        <label class="flex flex-col gap-1">
          <span class="type-section-label text-muted-foreground">Topic</span>
          <textarea v-model="editTopic" rows="3" class="chat-field-control chat-textarea-control type-field" />
        </label>
        <div class="flex flex-col gap-1.5 rounded-lg border border-border bg-muted/30 px-3 py-2.5">
          <label class="flex items-center gap-2 cursor-pointer">
            <input v-model="editIsPublic" type="checkbox" />
            <span class="type-control">Public</span>
          </label>
          <p class="type-caption text-muted-foreground">
            Anyone in the community can join. Uncheck to require explicit
            membership.
          </p>
        </div>
        <div v-if="editError" class="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 type-caption text-destructive" role="alert">{{ editError }}</div>
        <div class="flex justify-end">
          <button
            type="button"
            class="chat-action-button chat-action-button--primary type-action disabled:opacity-30"
            :disabled="editing || !editName.trim()"
            @click="saveConfig"
          >
            {{ editing ? "Saving…" : "Save changes" }}
          </button>
        </div>

        <div class="flex flex-col gap-2 border-t border-border pt-4 mt-2">
          <h3 class="type-section-label text-destructive">Danger zone</h3>
          <p class="type-caption text-muted-foreground">
            Deleting this channel destroys the MUC room. Occupants are
            ejected and history may be lost.
          </p>
          <div v-if="deleteError" class="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 type-caption text-destructive" role="alert">{{ deleteError }}</div>
          <button
            type="button"
            class="chat-action-button chat-action-button--destructive type-action"
            @click="showDelete = true"
          >
            Delete channel
          </button>
        </div>
      </section>

      <!-- Affiliations tab -->
      <section v-else-if="tab === 'affiliations'" class="flex flex-col gap-2">
        <div v-if="affiliationsError" class="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 type-caption text-destructive" role="alert">{{ affiliationsError }}</div>
        <p v-if="affiliationsLoading" class="type-caption text-muted-foreground">Loading…</p>
        <template v-else>
          <AffiliationRow
            v-for="entry in affiliations"
            :key="entry.jid"
            :entry="entry"
            :mutating="affiliationMutating === entry.jid"
            @change="(value) => changeAffiliation(entry, value)"
          />
          <p v-if="affiliations.length === 0" class="type-caption text-muted-foreground">No persistent affiliations set.</p>
        </template>
      </section>

      <!-- Occupants tab -->
      <section v-else class="flex flex-col gap-2">
        <div v-if="occupantsError" class="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 type-caption text-destructive" role="alert">{{ occupantsError }}</div>
        <p v-if="occupantsLoading" class="type-caption text-muted-foreground">Loading…</p>
        <template v-else>
          <OccupantRow
            v-for="entry in occupants"
            :key="entry.real_jid"
            :entry="entry"
            :kicking="occupantKicking === entry.real_jid"
            @kick="kickOccupant(entry)"
          />
          <p v-if="occupants.length === 0" class="type-caption text-muted-foreground">No one is here right now.</p>
        </template>
      </section>
    </div>

    <ConfirmDialog
      v-model:open="showDelete"
      title="Delete channel?"
      :message="`This destroys the room '${channel.name}' (${channel.channel_jid}). Occupants are ejected.`"
      confirm-label="Delete"
      destructive
      :loading="deleting"
      @confirm="confirmDelete"
    />
  </AppDrawer>
</template>
