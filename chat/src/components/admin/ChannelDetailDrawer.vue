<script setup lang="ts">
import AppSelect from "@/components/ui/AppSelect.vue";
// Admin V2 — slide-from-right drawer for editing a channel. Three
// tabs (Config / Affiliations / Occupants) + a danger section with a
// destroy-room confirm flow.
import { computed, onMounted, ref, watch } from "vue";
import { css, cx } from "styled-system/css";
import { btn, tab as tabRecipe, tabs as tabsRecipe } from "styled-system/recipes";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import { adminClasses as a } from "@/components/admin/admin-classes";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import AffiliationRow from "@/components/admin/AffiliationRow.vue";
import OccupantRow from "@/components/admin/OccupantRow.vue";
import { requireMembershipForUnlistedChannel } from "./channelAdmissionPolicy";
import type { BrowserXmppClient } from "@/lib/xmpp";
import type {
  WasmAdminChannelAffiliationEntry,
  WasmAdminChannelListEntry,
  WasmAdminChannelOccupantEntry,
  WasmAdminChannelType,
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
const CHANNEL_TYPE_OPTIONS: Array<{ value: WasmAdminChannelType; label: string }> = [
  { value: "text", label: "Text" },
  { value: "announcement", label: "Announcement" },
  { value: "forum", label: "Forum" },
];
const editChannelType = ref<WasmAdminChannelType>(props.channel.channel_type);
const editIsPublic = ref(props.channel.is_public);
const editMembersOnly = ref(props.channel.members_only);
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
  editChannelType.value = c.channel_type;
  editIsPublic.value = c.is_public;
  editMembersOnly.value = c.members_only;
  void loadAffiliations();
  void loadOccupants();
}, { immediate: false });

function handlePublicToggleChange() {
  const policy = requireMembershipForUnlistedChannel({
    isPublic: editIsPublic.value,
    membersOnly: editMembersOnly.value,
  });
  editIsPublic.value = policy.isPublic;
  editMembersOnly.value = policy.membersOnly;
}

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
      channelType: editChannelType.value,
      isPublic: editIsPublic.value,
      membersOnly: editMembersOnly.value,
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
    await loadAffiliations();
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
  { value: "config", label: "Settings" },
  { value: "affiliations", label: "Affiliations" },
  { value: "occupants", label: "Occupants" },
];

const tabsClass = cx(tabsRecipe(), css({ flexWrap: "wrap" }));
const tabClass = tabRecipe();
const tabPanelClass = css({ display: "flex", flexDirection: "column", gap: "14px" });

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
  <AppDrawer v-model:open="openLocal" side="right" label="Channel details" :width-class="a.drawerWidth">
    <template #title>
      <span :class="a.drawerTitle">{{ channel.name }}</span>
    </template>

    <div :class="a.drawerBody">
      <div :class="tabsClass" role="tablist" aria-label="Channel details">
        <button
          v-for="t in tabs"
          :key="t.value"
          type="button"
          role="tab"
          :class="tabClass"
          :aria-selected="tab === t.value"
          @click="tab = t.value"
        >
          {{ t.label }}<template v-if="t.value === 'affiliations'">{{ affiliationCountLabel }}</template>
          <template v-else-if="t.value === 'occupants'">{{ occupantCountLabel }}</template>
        </button>
      </div>

      <!-- Settings tab -->
      <section v-if="tab === 'config'" :class="tabPanelClass">
        <label :class="a.field">
          Name
          <input v-model="editName" :class="a.input" type="text" maxlength="80" />
        </label>
        <label :class="a.field">
          Topic
          <textarea v-model="editTopic" :class="a.textarea" rows="3" />
        </label>
        <label :class="a.field">
          Type
          <AppSelect v-model="editChannelType" :options="CHANNEL_TYPE_OPTIONS" :class="a.input" />
        </label>
        <div :class="a.checks">
          <label>
            <input v-model="editIsPublic" type="checkbox" @change="handlePublicToggleChange" />
            Listed in discovery
          </label>
          <label>
            <input v-model="editMembersOnly" type="checkbox" />
            Require explicit membership
          </label>
          <p>Discovery visibility and room admission are separate XMPP room settings.</p>
        </div>
        <div v-if="editError" :class="a.alert" role="alert">{{ editError }}</div>
        <button
          type="button"
          :class="[btn({ variant: 'primary' }), a.start]"
          :disabled="editing || !editName.trim()"
          @click="saveConfig"
        >
          {{ editing ? "Saving…" : "Save changes" }}
        </button>

        <div :class="a.danger">
          <h3>Delete channel</h3>
          <p>Everyone in it is removed and its history may be lost.</p>
          <div v-if="deleteError" :class="a.alert" role="alert">{{ deleteError }}</div>
          <button
            type="button"
            :class="[btn({ variant: 'danger', size: 'sm' }), a.start]"
            @click="showDelete = true"
          >
            Delete channel
          </button>
        </div>
      </section>

      <!-- Affiliations tab -->
      <section v-else-if="tab === 'affiliations'" :class="a.plain">
        <div v-if="affiliationsError" :class="a.alert" role="alert">{{ affiliationsError }}</div>
        <p v-if="affiliationsLoading" :class="a.sub">Loading…</p>
        <template v-else>
          <AffiliationRow
            v-for="entry in affiliations"
            :key="entry.jid"
            :entry="entry"
            :mutating="affiliationMutating === entry.jid"
            @change="(value) => changeAffiliation(entry, value)"
          />
          <p v-if="affiliations.length === 0" :class="a.sub">No persistent affiliations set.</p>
        </template>
      </section>

      <!-- Occupants tab -->
      <section v-else :class="a.plain">
        <div v-if="occupantsError" :class="a.alert" role="alert">{{ occupantsError }}</div>
        <p v-if="occupantsLoading" :class="a.sub">Loading…</p>
        <template v-else>
          <OccupantRow
            v-for="entry in occupants"
            :key="entry.real_jid"
            :entry="entry"
            :kicking="occupantKicking === entry.real_jid"
            @kick="kickOccupant(entry)"
          />
          <p v-if="occupants.length === 0" :class="a.sub">No one is here right now.</p>
        </template>
      </section>
    </div>

    <ConfirmDialog
      v-model:open="showDelete"
      title="Delete channel?"
      :message="`Everyone in ${channel.name} is removed. This can't be undone.`"
      confirm-label="Delete"
      destructive
      :loading="deleting"
      @confirm="confirmDelete"
    />
  </AppDrawer>
</template>
