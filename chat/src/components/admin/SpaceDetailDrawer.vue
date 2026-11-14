<script setup lang="ts">
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

const showDelete = ref(false);
const deleting = ref(false);
const deleteError = ref("");

watch(() => props.space, (s) => {
  editName.value = s.name;
  editDescription.value = s.description ?? "";
  editIconUrl.value = s.icon_url ?? "";
  void loadMembers();
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

onMounted(() => { void loadMembers(); });

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

      <section :class="a.section">
        <h3>Members</h3>
        <div v-if="membersError" :class="a.alert" role="alert">{{ membersError }}</div>
        <p v-if="membersLoading" :class="a.sub">Loading…</p>
        <template v-else-if="members.length > 0">
          <ul :class="a.plain" role="list">
            <li v-for="m in members" :key="m.jid" :class="a.subRow">
              <span :class="[a.jid, a.grow]">{{ m.jid }}</span>
              <select
                :value="m.role"
                :disabled="memberMutating === m.jid || membersLoadingMore || membersLoading"
                :class="a.smallSelect"
                :aria-label="`Role for ${m.jid}`"
                @change="changeMemberRole(m, ($event.target as HTMLSelectElement).value as Role)"
              >
                <option v-for="r in ROLES" :key="r" :value="r">{{ r }}</option>
              </select>
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
