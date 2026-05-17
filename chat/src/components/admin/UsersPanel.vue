<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { Search } from "lucide-vue-next";
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
const showEmptyState = computed(() => !isLoading.value && entries.value.length === 0 && !errorMessage.value);
</script>

<template>
  <section class="users-panel" aria-labelledby="users-panel-heading">
    <header class="users-panel-header">
      <h2 id="users-panel-heading" class="users-panel-title">Users</h2>
      <p class="users-panel-subtitle">
        Browse every account on this server. Search filters on the local part
        of the JID and display name. Server-side caps each page at 200 entries.
      </p>
      <div class="users-panel-search">
        <label class="visually-hidden" for="users-prefix">Search users</label>
        <span class="users-panel-search-icon" aria-hidden="true"><Search :size="16" /></span>
        <input
          id="users-prefix"
          v-model="prefix"
          type="search"
          autocomplete="off"
          placeholder="Search by username or display name"
          aria-label="Filter users by prefix"
        />
      </div>
    </header>

    <div v-if="errorMessage" class="users-panel-error" role="alert">{{ errorMessage }}</div>

    <div v-if="isLoading && entries.length === 0" class="users-panel-loading" role="status">
      Loading users…
    </div>

    <ul v-else-if="entries.length > 0" class="users-list" role="list">
      <li v-for="entry in entries" :key="entry.jid" class="users-list-row">
        <div class="users-list-row-main">
          <span class="users-list-row-name">
            {{ entry.display_name || entry.jid.split('@')[0] }}
          </span>
          <span class="users-list-row-jid">{{ entry.jid }}</span>
        </div>
        <span v-if="entry.has_owner_hat" class="users-list-row-owner-pill" title="Community owner">
          Owner
        </span>
      </li>
    </ul>

    <div v-else-if="showEmptyState" class="users-panel-empty" role="status">
      <p v-if="prefix">No users match “{{ prefix }}”.</p>
      <p v-else>No users registered yet.</p>
    </div>

    <footer v-if="hasMore" class="users-panel-footer">
      <button
        type="button"
        class="users-panel-more"
        :disabled="isLoadingMore"
        @click="loadMore"
      >
        {{ isLoadingMore ? "Loading…" : "Load more" }}
      </button>
    </footer>
  </section>
</template>

<style scoped>
.users-panel {
  max-width: 56rem;
  display: flex;
  flex-direction: column;
  gap: 1rem;
}

.users-panel-header {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.users-panel-title {
  font-size: 1.4rem;
  font-weight: 600;
  letter-spacing: -0.01em;
  margin: 0;
}

.users-panel-subtitle {
  font-size: 0.9rem;
  color: var(--muted-foreground, rgba(15, 18, 25, 0.6));
  margin: 0;
  max-width: 42rem;
}

.users-panel-search {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  border: 1px solid var(--border, rgba(15, 18, 25, 0.12));
  border-radius: 0.5rem;
  padding: 0 0.625rem;
  background: var(--card, #ffffff);
  max-width: 28rem;
}

.users-panel-search-icon { color: var(--muted-foreground, rgba(15, 18, 25, 0.45)); }

.users-panel-search input {
  flex: 1;
  border: 0;
  background: transparent;
  padding: 0.5rem 0;
  font: inherit;
  color: inherit;
  outline: none;
}

.users-panel-error {
  background: rgba(217, 32, 39, 0.08);
  color: rgb(160, 22, 28);
  border-radius: 0.5rem;
  padding: 0.625rem 0.75rem;
  font-size: 0.85rem;
}

.users-panel-loading,
.users-panel-empty {
  text-align: center;
  color: var(--muted-foreground, rgba(15, 18, 25, 0.6));
  padding: 3rem 0;
}

.users-list {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  border-radius: 0.625rem;
  border: 1px solid var(--border, rgba(15, 18, 25, 0.08));
  background: var(--card, #ffffff);
  overflow: hidden;
}

.users-list-row {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  padding: 0.625rem 0.875rem;
}

.users-list-row + .users-list-row { border-top: 1px solid var(--border, rgba(15, 18, 25, 0.06)); }

.users-list-row-main {
  display: flex;
  flex-direction: column;
  gap: 0.1rem;
  flex: 1;
  min-width: 0;
}

.users-list-row-name {
  font-weight: 500;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.users-list-row-jid {
  font-size: 0.78rem;
  color: var(--muted-foreground, rgba(15, 18, 25, 0.55));
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}

.users-list-row-owner-pill {
  font-size: 0.65rem;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  background: rgba(53, 99, 233, 0.12);
  color: rgb(31, 58, 163);
  padding: 0.15rem 0.5rem;
  border-radius: 999px;
}

.users-panel-footer { display: flex; justify-content: center; }

.users-panel-more {
  padding: 0.45rem 0.875rem;
  border-radius: 0.5rem;
  border: 1px solid var(--border, rgba(15, 18, 25, 0.12));
  background: var(--card, #ffffff);
  font: inherit;
  cursor: pointer;
}

.users-panel-more:disabled { opacity: 0.6; cursor: progress; }

.visually-hidden {
  position: absolute !important;
  width: 1px; height: 1px;
  margin: -1px; padding: 0;
  border: 0; clip: rect(0 0 0 0);
  overflow: hidden;
}
</style>
