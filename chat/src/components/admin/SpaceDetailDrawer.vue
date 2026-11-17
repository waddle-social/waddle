<script setup lang="ts">
import AppSelect from "@/components/ui/AppSelect.vue";
// Admin V2 — slide-from-right drawer for editing a single space.
// Three sections: editor, members, danger (delete) (Admin-Spaces board).
import { computed, onMounted, ref, watch } from "vue";
import { btn } from "styled-system/recipes";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import { adminClasses as a } from "@/components/admin/admin-classes";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import type { BrowserXmppClient } from "@/lib/xmpp";
import type {
  WasmAdminSpaceListEntry,
  WasmAdminSpaceMemberEntry,
} from "@/lib/xmpp";
import { jidDomain } from "@/lib/xmpp/jid";
import { fetchSpaceFederated, setSpaceFederated } from "@/lib/xmpp/federated-settings";
import { classifyRemoteError } from "@/lib/xmpp/remote-outcome";

const props = defineProps<{
  open: boolean;
  xmppClient: BrowserXmppClient | null;
  space: WasmAdminSpaceListEntry;
}>();

const emit = defineEmits<{
  close: [];
  changed: [];
  deleted: [];
}>();

type Role = "owner" | "admin" | "member" | "none";
const ROLES: Role[] = ["owner", "admin", "member", "none"];
const MEMBERS_PAGE_SIZE = 200;

const openLocal = ref(props.open);
watch(() => props.open, (v) => { openLocal.value = v; });
watch(openLocal, (v) => { if (!v) emit("close"); });

const editName = ref(props.space.name);
const editDescription = ref(props.space.description ?? "");
const editIconUrl = ref(props.space.icon_url ?? "");
const editing = ref(false);
const editError = ref("");

const members = ref<WasmAdminSpaceMemberEntry[]>([]);
const membersLoading = ref(false);
const membersLoadingMore = ref(false);
const memberCursor = ref<string | null>(null);
const membersError = ref("");
const memberMutating = ref<string | null>(null);
let membersRequestId = 0;

// Whether accounts on other servers can reach this Space, as its server
// holds it: `undefined` while loading, `null` when the server does not
// offer the setting.
const federated = ref<boolean | null | undefined>(undefined);
const federatedSaving = ref(false);
const federatedError = ref("");
// Bumped whenever a read or a change settles: the checkbox is redrawn from
// the value the server holds, even when that value did not change and the
// box was toggled by the click.
const federatedRenderKey = ref(0);
let federatedRequestId = 0;

/** The Spaces service: the domain of the Space's address. */
function spacesService(): string {
  return jidDomain(props.space.space_jid);
}

async function loadFederated() {
  const agent = props.xmppClient?.agent;
  const requestId = ++federatedRequestId;
  federatedError.value = "";
  if (!agent) {
    federated.value = null;
    return;
  }
  federated.value = undefined;
  try {
    const value = await fetchSpaceFederated(agent, spacesService(), props.space.space_node);
    if (requestId === federatedRequestId) {
      federated.value = value;
      federatedRenderKey.value += 1;
    }
  } catch (err: unknown) {
    if (requestId !== federatedRequestId) return;
    federated.value = null;
    // Only the node's owner reads its configure form; for anyone else the
    // setting is simply not offered.
    if (classifyRemoteError(err).kind === "denial") return;
    federatedError.value = err instanceof Error ? err.message : "Couldn't read whether this Space is open to other servers.";
  }
}

async function changeFederated(next: boolean) {
  const agent = props.xmppClient?.agent;
  if (!agent || federatedSaving.value) return;
  const requestId = ++federatedRequestId;
  federatedSaving.value = true;
  federatedError.value = "";
  try {
    const { current, error } = await setSpaceFederated(agent, spacesService(), props.space.space_node, next);
    if (requestId !== federatedRequestId) return;
    // Show what the server holds now, whatever was submitted.
    federated.value = current;
    if (error || current !== next) {
      federatedError.value = "The server did not apply the change. The setting shows its current value.";
    }
  } catch (err: unknown) {
    if (requestId !== federatedRequestId) return;
    federatedError.value = err instanceof Error ? err.message : "Couldn't change whether this Space is open to other servers.";
  } finally {
    // Whatever settled the request (its answer, or a newer read of another
    // Space that superseded it), the setting can be changed again.
    federatedSaving.value = false;
    federatedRenderKey.value += 1;
  }
}

const showDelete = ref(false);
const deleting = ref(false);
const deleteError = ref("");

watch(() => props.space, (s) => {
  editName.value = s.name;
  editDescription.value = s.description ?? "";
  editIconUrl.value = s.icon_url ?? "";
  void loadMembers();
  void loadFederated();
}, { immediate: false });

async function loadMembers() {
  if (!props.xmppClient) return;
  const requestId = ++membersRequestId;
  membersLoading.value = true;
  membersLoadingMore.value = false;
  membersError.value = "";
  memberCursor.value = null;
  try {
    const page = await props.xmppClient.adminSpacesMembers({
      spaceJid: props.space.space_jid,
      spaceNode: props.space.space_node,
      pageSize: MEMBERS_PAGE_SIZE,
    });
    if (requestId !== membersRequestId) return;
    members.value = page.entries;
    memberCursor.value = page.next_cursor ?? null;
  } catch (err: unknown) {
    if (requestId !== membersRequestId) return;
    membersError.value = err instanceof Error ? err.message : "Failed to load members.";
  } finally {
    if (requestId === membersRequestId) {
      membersLoading.value = false;
    }
  }
}

async function loadMoreMembers() {
  if (!props.xmppClient || !memberCursor.value || membersLoadingMore.value) return;
  const requestId = membersRequestId;
  const afterCursor = memberCursor.value;
  membersLoadingMore.value = true;
  membersError.value = "";
  try {
    const page = await props.xmppClient.adminSpacesMembers({
      spaceJid: props.space.space_jid,
      spaceNode: props.space.space_node,
      pageSize: MEMBERS_PAGE_SIZE,
      afterCursor,
    });
    if (requestId !== membersRequestId || memberCursor.value !== afterCursor) return;
    members.value = members.value.concat(page.entries);
    memberCursor.value = page.next_cursor ?? null;
  } catch (err: unknown) {
    if (requestId !== membersRequestId) return;
    membersError.value = err instanceof Error ? err.message : "Failed to load more members.";
  } finally {
    membersLoadingMore.value = false;
  }
}

async function saveEdits() {
  if (!props.xmppClient) return;
  if (!editName.value.trim()) return;
  editing.value = true;
  editError.value = "";
  try {
    await props.xmppClient.adminSpacesUpdate({
      spaceJid: props.space.space_jid,
      spaceNode: props.space.space_node,
      name: editName.value.trim(),
      description: editDescription.value.trim() || null,
      iconUrl: editIconUrl.value.trim() || null,
    });
    emit("changed");
  } catch (err: unknown) {
    editError.value = err instanceof Error ? err.message : "Failed to update space.";
  } finally {
    editing.value = false;
  }
}

async function changeMemberRole(member: WasmAdminSpaceMemberEntry, newRole: Role) {
  if (!props.xmppClient || membersLoading.value || membersLoadingMore.value) return;
  memberMutating.value = member.jid;
  try {
    await props.xmppClient.adminSpacesSetRole({
      spaceJid: props.space.space_jid,
      spaceNode: props.space.space_node,
      memberJid: member.jid,
      role: newRole,
    });
    await loadMembers();
    emit("changed");
  } catch (err: unknown) {
    membersError.value = err instanceof Error ? err.message : "Failed to update role.";
  } finally {
    memberMutating.value = null;
  }
}

async function confirmDelete() {
  if (!props.xmppClient) return;
  deleting.value = true;
  deleteError.value = "";
  try {
    await props.xmppClient.adminSpacesDelete({
      spaceJid: props.space.space_jid,
      spaceNode: props.space.space_node,
    });
    showDelete.value = false;
    emit("deleted");
  } catch (err: unknown) {
    deleteError.value = err instanceof Error ? err.message : "Failed to delete space.";
  } finally {
    deleting.value = false;
  }
}

onMounted(() => {
  void loadMembers();
  void loadFederated();
});

const hasMoreMembers = computed(() => memberCursor.value !== null);
</script>

<template>
  <AppDrawer v-model:open="openLocal" side="right" label="Space details" :width-class="a.drawerWidth">
    <template #title>
      <span :class="a.drawerTitle">{{ space.name }}</span>
    </template>
    <div :class="a.drawerBody">
      <label :class="a.field">
        Name
        <input v-model="editName" :class="a.input" type="text" maxlength="80" />
      </label>
      <label :class="a.field">
        Description
        <textarea v-model="editDescription" :class="a.textarea" rows="2" />
      </label>
      <label :class="a.field">
        Icon URL
        <input v-model="editIconUrl" :class="a.input" type="url" placeholder="https://…" />
      </label>
      <div v-if="editError" :class="a.alert" role="alert">{{ editError }}</div>
      <button
        type="button"
        :class="[btn({ variant: 'primary' }), a.start]"
        :disabled="editing || !editName.trim()"
        @click="saveEdits"
      >
        {{ editing ? "Saving…" : "Save changes" }}
      </button>

      <!-- Other servers: shown when the Space's configure form offers the setting -->
      <section v-if="federated !== null || federatedError" :class="a.section">
        <h3>Other servers</h3>
        <div :class="a.checks">
          <p v-if="federated === undefined">Loading…</p>
          <label v-else-if="federated !== null">
            <input
              :key="federatedRenderKey"
              type="checkbox"
              :checked="federated"
              :disabled="federatedSaving"
              aria-describedby="space-federated-help"
              @change="changeFederated(($event.target as HTMLInputElement).checked)"
            />
            <span>Open this Space to accounts on other servers</span>
          </label>
          <p id="space-federated-help">
            Off by default. When on, accounts on every server this one connects to can see the Space, follow
            it and read which of its rooms are open to them. Following a Space gives no access to its rooms:
            each room has its own setting and members. Turning it off closes the Space to other servers and ends
            their subscriptions.
          </p>
        </div>
        <div v-if="federatedError" :class="a.alert" role="alert">{{ federatedError }}</div>
      </section>

      <section :class="a.section">
        <h3>Members</h3>
        <div v-if="membersError" :class="a.alert" role="alert">{{ membersError }}</div>
        <p v-if="membersLoading" :class="a.sub">Loading…</p>
        <template v-else-if="members.length > 0">
          <ul :class="a.plain" role="list">
            <li v-for="m in members" :key="m.jid" :class="a.subRow">
              <span :class="[a.jid, a.grow]">{{ m.jid }}</span>
              <AppSelect
                :model-value="m.role"
                :options="ROLES.map((r) => ({ value: r, label: r }))"
                :disabled="memberMutating === m.jid || membersLoadingMore || membersLoading"
                :class="a.smallSelect"
                :aria-label="`Role for ${m.jid}`"
                @update:model-value="changeMemberRole(m, $event)"
              />
            </li>
          </ul>
          <button
            v-if="hasMoreMembers"
            type="button"
            :class="[btn({ size: 'sm' }), a.start]"
            :disabled="membersLoadingMore || memberMutating !== null || membersLoading"
            @click="loadMoreMembers"
          >
            {{ membersLoadingMore ? "Loading…" : "Load more members" }}
          </button>
        </template>
        <p v-else :class="a.sub">No members yet.</p>
      </section>

      <section :class="a.section">
        <div :class="a.danger">
          <h3>Delete space</h3>
          <p>
            This also deletes its
            <strong>{{ space.channel_count }}</strong>
            {{ space.channel_count === 1 ? "channel" : "channels" }}. This can't be undone.
          </p>
          <div v-if="deleteError" :class="a.alert" role="alert">{{ deleteError }}</div>
          <button
            type="button"
            :class="[btn({ variant: 'danger', size: 'sm' }), a.start]"
            @click="showDelete = true"
          >
            Delete space
          </button>
        </div>
      </section>
    </div>

    <ConfirmDialog
      v-model:open="showDelete"
      title="Delete space?"
      :message="`This deletes ${space.name} and its ${space.channel_count} channel${space.channel_count === 1 ? '' : 's'}. This can't be undone.`"
      confirm-label="Delete"
      destructive
      :loading="deleting"
      @confirm="confirmDelete"
    />
  </AppDrawer>
</template>
