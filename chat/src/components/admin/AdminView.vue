<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { ChevronLeft } from "lucide-vue-next";
import AdminLayout from "@/components/admin/AdminLayout.vue";
import UsersPanel from "@/components/admin/UsersPanel.vue";
import SpacesPanel from "@/components/admin/SpacesPanel.vue";
import ChannelsPanel from "@/components/admin/ChannelsPanel.vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { AdminPanel } from "@/router";

type RoleCheckState = "loading" | "owner" | "denied" | "error";

const props = defineProps<{
  xmppClient: BrowserXmppClient | null;
  /** Slug of the currently-rendered admin panel; defaults to "users". */
  activePanel: AdminPanel;
}>();

const emit = defineEmits<{
  /** Navigate to a different admin panel path (`/admin/users`, …). */
  navigate: [path: string];
  /** Leave the admin view entirely. The handler pops history back to chat. */
  back: [];
}>();

const roleState = ref<RoleCheckState>("loading");

async function checkRole(): Promise<void> {
  if (!props.xmppClient) {
    roleState.value = "error";
    return;
  }
  try {
    const isOwner = await props.xmppClient.isCommunityOwner();
    roleState.value = isOwner ? "owner" : "denied";
  } catch {
    roleState.value = "error";
  }
}

async function loadUsers(opts: { prefix?: string | null; afterCursor?: string | null }) {
  if (!props.xmppClient) throw new Error("XMPP client is not connected");
  return await props.xmppClient.adminUsersList({
    prefix: opts.prefix ?? null,
    pageSize: 50,
    afterCursor: opts.afterCursor ?? null,
  });
}

const isLoading = computed(() => roleState.value === "loading");
const isOwner = computed(() => roleState.value === "owner");
const showDenied = computed(() => roleState.value === "denied" || roleState.value === "error");
const deniedMessage = computed(() =>
  roleState.value === "error"
    ? "Couldn't verify your admin access. Please try again."
    : "Admin access only. Ask a community owner if you need to manage this server.",
);

onMounted(() => {
  void checkRole();
});
</script>

<template>
  <div v-if="isLoading" class="admin-role-loading" role="status">Checking admin access…</div>

  <div v-else-if="showDenied" class="admin-denied" role="status">
    <h1>Admin access only</h1>
    <p>{{ deniedMessage }}</p>
    <button type="button" class="admin-denied-back" @click="emit('back')">
      <ChevronLeft :size="16" aria-hidden="true" />
      <span>Back to chat</span>
    </button>
  </div>

  <AdminLayout
    v-else-if="isOwner"
    :active-panel="activePanel"
    @navigate="(path: string) => emit('navigate', path)"
    @back="emit('back')"
  >
    <template #users>
      <UsersPanel :load-page="loadUsers" />
    </template>
    <template #spaces>
      <SpacesPanel :xmpp-client="xmppClient" />
    </template>
    <template #channels>
      <ChannelsPanel :xmpp-client="xmppClient" />
    </template>
  </AdminLayout>
</template>

<style scoped>
.admin-role-loading,
.admin-denied {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  height: 100dvh;
  background: var(--background, #f4f5f7);
  color: var(--foreground, #0c0d12);
  gap: 0.75rem;
  text-align: center;
  padding: 2rem;
}

.admin-denied h1 {
  font-size: 1.5rem;
  font-weight: 600;
  letter-spacing: -0.01em;
  margin: 0;
}

.admin-denied p {
  font-size: 0.95rem;
  color: var(--muted-foreground, rgba(15, 18, 25, 0.65));
  max-width: 28rem;
  margin: 0;
  line-height: 1.5;
}

.admin-denied-back {
  display: inline-flex;
  align-items: center;
  gap: 0.25rem;
  margin-top: 0.5rem;
  padding: 0.45rem 0.875rem;
  border-radius: 0.5rem;
  border: 1px solid var(--border, rgba(15, 18, 25, 0.12));
  background: var(--card, #ffffff);
  font: inherit;
  cursor: pointer;
}
</style>
