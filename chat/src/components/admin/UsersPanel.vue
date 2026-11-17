<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { css, cx } from "styled-system/css";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { adminClasses as a } from "@/components/admin/admin-classes";
import type { AdminUserEntry, AdminUsersPage } from "@/lib/xmpp";

const props = defineProps<{
  /**
   * Page loader. The component owns query state (prefix, cursor) and
   * delegates the actual network call to the parent. The parent is
   * responsible for plumbing `xmppClient.adminUsersList(...)` here so
   * Vitest can substitute a fake without spinning up the wasm client.
   */
  loadPage: (opts: { prefix?: string | null; afterCursor?: string | null }) => Promise<AdminUsersPage>;
}>();

const SEARCH_DEBOUNCE_MS = 200;

const prefix = ref<string>("");
const debouncedPrefix = ref<string>("");
const entries = ref<AdminUserEntry[]>([]);
const cursor = ref<string | null>(null);
const isLoading = ref<boolean>(false);
const isLoadingMore = ref<boolean>(false);
const errorMessage = ref<string>("");
let debounceTimer: ReturnType<typeof setTimeout> | null = null;
let requestId = 0;

async function fetchFirstPage(currentPrefix: string): Promise<void> {
  const localRequestId = ++requestId;
  isLoading.value = true;
  errorMessage.value = "";
  try {
    const page = await props.loadPage({ prefix: currentPrefix || null });
    if (requestId !== localRequestId) return;
    entries.value = page.entries;
    cursor.value = page.next_cursor ?? null;
  } catch (err: unknown) {
    if (requestId !== localRequestId) return;
    errorMessage.value = err instanceof Error ? err.message : "Failed to load users.";
  } finally {
    if (requestId === localRequestId) isLoading.value = false;
  }
}

async function loadMore(): Promise<void> {
  if (!cursor.value || isLoadingMore.value) return;
  isLoadingMore.value = true;
  try {
    const page = await props.loadPage({ prefix: prefix.value || null, afterCursor: cursor.value });
    entries.value = entries.value.concat(page.entries);
    cursor.value = page.next_cursor ?? null;
  } catch (err: unknown) {
    errorMessage.value = err instanceof Error ? err.message : "Failed to load more users.";
  } finally {
    isLoadingMore.value = false;
  }
}

watch(prefix, (value) => {
  if (debounceTimer) clearTimeout(debounceTimer);
  debounceTimer = setTimeout(() => {
    debouncedPrefix.value = value;
  }, SEARCH_DEBOUNCE_MS);
});

watch(debouncedPrefix, (value) => {
  void fetchFirstPage(value);
});

onMounted(() => {
  void fetchFirstPage("");
});

const hasMore = computed(() => cursor.value !== null);
const panelClass = css({ display: "flex", flexDirection: "column", gap: "4" });
const ownerClass = cx(a.tag, css({ color: "gold.text", borderColor: "gold" }));

const showEmptyState = computed(() => !isLoading.value && entries.value.length === 0 && !errorMessage.value);
</script>

<template>
  <section :class="panelClass" aria-label="Users">
    <label :class="a.srOnly" for="users-prefix">Search users</label>
    <input
      id="users-prefix"
      v-model="prefix"
      :class="a.search"
      type="search"
      autocomplete="off"
      placeholder="Search by username or display name"
      aria-label="Filter users by prefix"
    />

    <div v-if="errorMessage" :class="a.alert" role="alert">{{ errorMessage }}</div>

    <div v-if="isLoading && entries.length === 0" :class="a.status" role="status">
      Loading users…
    </div>

    <ul v-else-if="entries.length > 0" :class="a.list" role="list">
      <li v-for="entry in entries" :key="entry.jid" :class="a.row">
        <UserAvatar :name="entry.display_name || entry.jid.split('@')[0] || entry.jid" :jid="entry.jid" size="sm" />
        <div :class="a.rowMain">
          <b :class="a.truncate">{{ entry.display_name || entry.jid.split('@')[0] }}</b>
          <span :class="a.jid">{{ entry.jid }}</span>
        </div>
        <span v-if="entry.has_owner_hat" :class="ownerClass" title="Community owner">Owner</span>
      </li>
    </ul>

    <div v-else-if="showEmptyState" :class="a.status" role="status">
      <p v-if="prefix">No users match “{{ prefix }}”.</p>
      <p v-else>No users registered yet.</p>
    </div>

    <button
      v-if="hasMore"
      type="button"
      :class="a.more"
      :disabled="isLoadingMore"
      @click="loadMore"
    >
      {{ isLoadingMore ? "Loading…" : "Load more" }}
    </button>
  </section>
</template>
