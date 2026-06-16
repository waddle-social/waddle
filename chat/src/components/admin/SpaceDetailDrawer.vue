<script setup lang="ts">
// Admin V2 — slide-from-right drawer for editing a single space.
// Three sections: editor, members, danger (delete).
import { computed, onMounted, ref, watch } from "vue";
import AppDrawer from "@/components/ui/AppDrawer.vue";
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
  <AppDrawer v-model:open="openLocal" side="right" label="Space details" width-class="w-full max-w-md lg:max-w-lg">
    <template #title>
      <span class="type-pane-title truncate">{{ space.name }}</span>
    </template>
    <div class="flex flex-col gap-6 p-4">
      <!-- Editor -->
      <section class="flex flex-col gap-3">
        <h3 class="type-section-label text-muted-foreground">Edit</h3>
        <label class="flex flex-col gap-1">
          <span class="type-section-label text-muted-foreground">Name</span>
          <input v-model="editName" type="text" maxlength="80" class="chat-field-control type-field" />
        </label>
        <label class="flex flex-col gap-1">
          <span class="type-section-label text-muted-foreground">Description</span>
          <textarea v-model="editDescription" rows="3" class="chat-field-control chat-textarea-control type-field" />
        </label>
        <label class="flex flex-col gap-1">
          <span class="type-section-label text-muted-foreground">Icon URL</span>
          <input v-model="editIconUrl" type="url" class="chat-field-control type-field" />
        </label>
        <div v-if="editError" class="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 type-caption text-destructive" role="alert">{{ editError }}</div>
        <div class="flex justify-end">
          <button
            type="button"
            class="chat-action-button chat-action-button--primary type-action disabled:opacity-30"
            :disabled="editing || !editName.trim()"
            @click="saveEdits"
          >
            {{ editing ? "Saving…" : "Save changes" }}
          </button>
        </div>
      </section>

      <!-- Members -->
      <section class="flex flex-col gap-3 border-t border-border pt-4">
        <h3 class="type-section-label text-muted-foreground">Members</h3>
        <div v-if="membersError" class="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 type-caption text-destructive" role="alert">{{ membersError }}</div>
        <div v-if="membersLoading" class="type-caption text-muted-foreground">Loading…</div>
        <div v-else-if="members.length > 0" class="flex flex-col gap-2">
          <ul class="flex flex-col gap-1.5" role="list">
            <li v-for="m in members" :key="m.jid" class="flex items-center gap-2 rounded-md border border-border bg-card px-2.5 py-2">
              <span class="flex-1 truncate font-mono type-caption">{{ m.jid }}</span>
              <select
                :value="m.role"
                :disabled="memberMutating === m.jid || membersLoadingMore || membersLoading"
                class="chat-field-control type-caption"
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
            class="chat-action-button type-action self-start disabled:opacity-30"
            :disabled="membersLoadingMore || memberMutating !== null || membersLoading"
            @click="loadMoreMembers"
          >
            {{ membersLoadingMore ? "Loading…" : "Load more members" }}
          </button>
        </div>
        <p v-else class="type-caption text-muted-foreground">No members yet.</p>
      </section>

      <!-- Danger -->
      <section class="flex flex-col gap-3 border-t border-border pt-4">
        <h3 class="type-section-label text-destructive">Danger zone</h3>
        <p class="type-caption text-muted-foreground">
          Deleting this space cascade-destroys
          <strong>{{ space.channel_count }}</strong>
          {{ space.channel_count === 1 ? "channel" : "channels" }} under it. This cannot be undone.
        </p>
        <div v-if="deleteError" class="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 type-caption text-destructive" role="alert">{{ deleteError }}</div>
        <button
          type="button"
          class="chat-action-button chat-action-button--destructive type-action"
          @click="showDelete = true"
        >
          Delete space
        </button>
      </section>
    </div>

    <ConfirmDialog
      v-model:open="showDelete"
      title="Delete space?"
      :message="`This will delete '${space.name}' and ${space.channel_count} channel${space.channel_count === 1 ? '' : 's'} under it. This cannot be undone.`"
      confirm-label="Delete"
      destructive
      :loading="deleting"
      @confirm="confirmDelete"
    />
  </AppDrawer>
</template>
