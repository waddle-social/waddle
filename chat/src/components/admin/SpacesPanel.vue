<script setup lang="ts">
// Admin V2 — Spaces panel. Mirrors the V1 `UsersPanel` shape (prefix
// search + paginated list) and layers in a "+" create button and a
// click-to-open detail drawer.
import { computed, onMounted, ref, watch } from "vue";
import { Plus } from "lucide-vue-next";
import { css } from "styled-system/css";
import { btn } from "styled-system/recipes";
import { adminClasses as a } from "@/components/admin/admin-classes";
import type { BrowserXmppClient } from "@/lib/xmpp";
import type {
  WasmAdminSpaceListEntry,
  WasmAdminSpacesListResult,
} from "@/lib/xmpp";
import SpaceCreateDialog from "@/components/admin/SpaceCreateDialog.vue";
import SpaceDetailDrawer from "@/components/admin/SpaceDetailDrawer.vue";

const props = defineProps<{
  xmppClient: BrowserXmppClient | null;
}>();

const SEARCH_DEBOUNCE_MS = 200;
const PAGE_SIZE = 50;

const entries = ref<WasmAdminSpaceListEntry[]>([]);
const cursor = ref<string | null>(null);
const isLoading = ref(false);
const isLoadingMore = ref(false);
const errorMessage = ref("");
const prefix = ref("");
const debouncedPrefix = ref("");
const activePrefix = ref("");
const showCreate = ref(false);
const isSubmitting = ref(false);
const selected = ref<WasmAdminSpaceListEntry | null>(null);
let debounceTimer: ReturnType<typeof setTimeout> | null = null;
let requestId = 0;

async function fetchFirstPage(currentPrefix: string): Promise<void> {
  if (!props.xmppClient) return;
  const localRequestId = ++requestId;
  isLoading.value = true;
  isLoadingMore.value = false;
  cursor.value = null;
  errorMessage.value = "";
  try {
    const page: WasmAdminSpacesListResult = await props.xmppClient.adminSpacesList({
      prefix: currentPrefix || null,
      pageSize: PAGE_SIZE,
    });
    if (requestId !== localRequestId) return;
    activePrefix.value = currentPrefix;
    entries.value = page.entries;
    cursor.value = page.next_cursor ?? null;
  } catch (err: unknown) {
    if (requestId !== localRequestId) return;
    errorMessage.value = err instanceof Error ? err.message : "Failed to load spaces.";
  } finally {
    if (requestId === localRequestId) isLoading.value = false;
  }
}

async function loadMore(): Promise<void> {
  if (!props.xmppClient || !cursor.value || isLoading.value || isLoadingMore.value) return;
  const localRequestId = requestId;
  const afterCursor = cursor.value;
  const currentPrefix = activePrefix.value;
  isLoadingMore.value = true;
  try {
    const page = await props.xmppClient.adminSpacesList({
      prefix: currentPrefix || null,
      pageSize: PAGE_SIZE,
      afterCursor,
    });
    if (
      requestId !== localRequestId ||
      cursor.value !== afterCursor ||
      activePrefix.value !== currentPrefix
    ) {
      return;
    }
    entries.value = entries.value.concat(page.entries);
    cursor.value = page.next_cursor ?? null;
  } catch (err: unknown) {
    if (requestId !== localRequestId) return;
    errorMessage.value = err instanceof Error ? err.message : "Failed to load more spaces.";
  } finally {
    isLoadingMore.value = false;
  }
}

async function onCreate(payload: { name: string; description?: string | null; iconUrl?: string | null }) {
  if (!props.xmppClient) return;
  isSubmitting.value = true;
  try {
    await props.xmppClient.adminSpacesCreate(payload);
    showCreate.value = false;
    await fetchFirstPage(prefix.value);
  } catch (err: unknown) {
    errorMessage.value = err instanceof Error ? err.message : "Failed to create space.";
  } finally {
    isSubmitting.value = false;
  }
}

function openDetail(entry: WasmAdminSpaceListEntry) {
  selected.value = entry;
}
function closeDetail() {
  selected.value = null;
}
async function onDetailChanged() {
  await fetchFirstPage(prefix.value);
}
async function onDetailDeleted() {
  selected.value = null;
  await fetchFirstPage(prefix.value);
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
const newClass = btn({ variant: "primary" });

const showEmptyState = computed(
  () => !isLoading.value && entries.value.length === 0 && !errorMessage.value,
);
</script>

<template>
  <section :class="panelClass" aria-label="Spaces">
    <Teleport defer to="#admin-header-actions">
      <button type="button" :class="newClass" aria-label="Create space" @click="showCreate = true">
        <Plus aria-hidden="true" />
        <span>New space</span>
      </button>
    </Teleport>
    <label :class="a.srOnly" for="spaces-prefix">Search spaces</label>
    <input
      id="spaces-prefix"
      v-model="prefix"
      :class="a.search"
      type="search"
      autocomplete="off"
      placeholder="Search spaces"
      aria-label="Filter spaces by prefix"
    />

    <div v-if="errorMessage" :class="a.alert" role="alert">{{ errorMessage }}</div>

    <div v-if="isLoading && entries.length === 0" :class="a.status" role="status">
      Loading spaces…
    </div>

    <ul v-else-if="entries.length > 0" :class="a.list" role="list">
      <li v-for="entry in entries" :key="entry.space_node">
        <button type="button" :class="a.row" :aria-label="`Open ${entry.name}`" @click="openDetail(entry)">
          <div :class="a.rowMain">
            <b :class="a.truncate">{{ entry.name }}</b>
            <span :class="a.jid">{{ entry.space_jid }}</span>
            <span v-if="entry.description" :class="a.sub">{{ entry.description }}</span>
          </div>
          <span :class="a.meta">{{ entry.channel_count }} {{ entry.channel_count === 1 ? "channel" : "channels" }} · {{ entry.member_count }} {{ entry.member_count === 1 ? "member" : "members" }}</span>
        </button>
      </li>
    </ul>

    <div v-else-if="showEmptyState" :class="a.status" role="status">
      <p v-if="prefix">No spaces match “{{ prefix }}”.</p>
      <p v-else>No spaces yet. Create one to get started.</p>
    </div>

    <button
      v-if="hasMore"
      type="button"
      :class="a.more"
      :disabled="isLoadingMore || isLoading"
      @click="loadMore"
    >
      {{ isLoadingMore ? "Loading…" : "Load more" }}
    </button>

    <SpaceCreateDialog
      v-model:open="showCreate"
      :is-submitting="isSubmitting"
      @submit="onCreate"
    />
    <SpaceDetailDrawer
      v-if="selected"
      :open="!!selected"
      :xmpp-client="xmppClient"
      :space="selected"
      @close="closeDetail"
      @changed="onDetailChanged"
      @deleted="onDetailDeleted"
    />
  </section>
</template>
