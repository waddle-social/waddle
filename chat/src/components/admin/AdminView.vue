<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { ChevronLeft } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { btn, empty, waddle } from "styled-system/recipes";
import spotEmpty from "@waddle/design/assets/spot-empty.svg?url";
import spotError from "@waddle/design/assets/spot-error.svg?url";
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
  /** Whether the home server federates with other servers. */
  federationAvailable: boolean;
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
    : "Ask a community owner if you need to manage this server.",
);

// Admin-Access board: a centred loader, or the empty state with one way back.
// The shell's main column is a flex column; the box fills it.
const centreClass = css({ display: "flex", alignItems: "center", justifyContent: "center", gap: "3", flex: 1, minHeight: 0, padding: "{spacing.8} {spacing.4}", boxSizing: "border-box", bg: "bg", color: "fg.muted", fontSize: "15px" });
const deniedClass = cx(empty(), css({ maxWidth: "420px", color: "fg", "& h1": { margin: 0, font: "700 24px/1.25 {fonts.display}" } }));
const backClass = btn();

onMounted(() => {
  void checkRole();
});
</script>

<template>
  <div v-if="isLoading">
    <div :class="centreClass" role="status">
      <span :class="waddle()" aria-hidden="true"><i /><i /><i /></span>
      Checking admin access…
    </div>
  </div>

  <div v-else-if="showDenied">
    <div :class="centreClass">
      <section :class="deniedClass" role="status">
        <img :src="roleState === 'error' ? spotError : spotEmpty" alt="" />
        <h1>Admin access only</h1>
        <p>{{ deniedMessage }}</p>
        <button type="button" :class="backClass" @click="emit('back')">
          <ChevronLeft aria-hidden="true" />
          <span>Back to chat</span>
        </button>
      </section>
    </div>
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
      <SpacesPanel :xmpp-client="xmppClient" :federation-available="federationAvailable" />
    </template>
    <template #channels>
      <ChannelsPanel :xmpp-client="xmppClient" />
    </template>
  </AdminLayout>
</template>
