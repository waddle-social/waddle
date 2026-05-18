<script setup lang="ts">
// Admin V2 — Spaces panel. Mirrors the V1 `UsersPanel` shape (prefix
// search + paginated list) and layers in a "+" create button and a
// click-to-open detail drawer.
import { computed, onMounted, ref, watch } from "vue";
import { Plus, Search } from "lucide-vue-next";
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
const showCreate = ref(false);
const isSubmitting = ref(false);
const selected = ref<WasmAdminSpaceListEntry | null>(null);
let debounceTimer: ReturnType<typeof setTimeout> | null = null;
let requestId = 0;

async function fetchFirstPage(currentPrefix: string): Promise<void> {
  if (!props.xmppClient) return;
  const localRequestId = ++requestId;
  isLoading.value = true;
  errorMessage.value = "";
  try {
    const page: WasmAdminSpacesListResult = await props.xmppClient.adminSpacesList({
      prefix: currentPrefix || null,
      pageSize: PAGE_SIZE,
    });
    if (requestId !== localRequestId) return;
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
  if (!props.xmppClient || !cursor.value || isLoadingMore.value) return;
  isLoadingMore.value = true;
  try {
    const page = await props.xmppClient.adminSpacesList({
      prefix: prefix.value || null,
      pageSize: PAGE_SIZE,
      afterCursor: cursor.value,
    });
    entries.value = entries.value.concat(page.entries);
    cursor.value = page.next_cursor ?? null;
  } catch (err: unknown) {
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
const showEmptyState = computed(
  () => !isLoading.value && entries.value.length === 0 && !errorMessage.value,
);
</script>

<template>
  <section class="flex flex-col gap-4 p-4 max-w-4xl mx-auto w-full" aria-labelledby="spaces-panel-heading">
    <header class="flex flex-col gap-2">
      <div class="flex items-center justify-between gap-3">
        <div class="flex flex-col gap-1">
          <h2 id="spaces-panel-heading" class="type-pane-title">Spaces</h2>
          <p class="type-caption text-muted-foreground max-w-prose">
            Create, edit, and delete community spaces. Cascade-deletes
            child channels.
          </p>
        </div>
        <button
          type="button"
          class="chat-action-button chat-action-button--primary type-action"
          aria-label="Create space"
          @click="showCreate = true"
        >
          <Plus class="w-4 h-4" />
          <span>New space</span>
        </button>
      </div>
      <div class="flex items-center gap-2 chat-field-control max-w-md">
        <Search class="w-4 h-4 text-muted-foreground" aria-hidden="true" />
        <input
          v-model="prefix"
          type="search"
          autocomplete="off"
          placeholder="Search spaces"
          aria-label="Filter spaces by prefix"
          class="flex-1 bg-transparent border-0 outline-none type-field"
        />
      </div>
    </header>

    <div
      v-if="errorMessage"
      class="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 type-caption text-destructive"
      role="alert"
    >
      {{ errorMessage }}
    </div>

    <div v-if="isLoading && entries.length === 0" class="py-12 text-center type-caption text-muted-foreground" role="status">
      Loading spaces…
    </div>

    <ul v-else-if="entries.length > 0" class="flex flex-col gap-2" role="list">
      <li v-for="entry in entries" :key="entry.space_jid">
        <button
          type="button"
          class="w-full flex items-center justify-between gap-3 rounded-lg border border-border bg-card px-3 py-2.5 text-left hover:bg-muted transition-colors"
          :aria-label="`Open ${entry.name}`"
          @click="openDetail(entry)"
        >
          <div class="flex flex-col gap-0.5 min-w-0 flex-1">
            <span class="type-control truncate">{{ entry.name }}</span>
            <span class="type-caption text-muted-foreground truncate font-mono">{{ entry.space_jid }}</span>
            <span v-if="entry.description" class="type-caption text-muted-foreground truncate">{{ entry.description }}</span>
          </div>
          <div class="flex items-center gap-2 flex-shrink-0">
            <span class="type-caption rounded-full bg-muted px-2 py-0.5">{{ entry.channel_count }} ch</span>
            <span class="type-caption rounded-full bg-muted px-2 py-0.5">{{ entry.member_count }} mem</span>
          </div>
        </button>
      </li>
    </ul>

    <div v-else-if="showEmptyState" class="py-12 text-center type-caption text-muted-foreground" role="status">
      <p v-if="prefix">No spaces match “{{ prefix }}”.</p>
      <p v-else>No spaces yet. Create one to get started.</p>
    </div>

    <div v-if="hasMore" class="flex justify-center pt-2">
      <button
        type="button"
        class="chat-action-button chat-action-button--secondary type-control"
        :disabled="isLoadingMore"
        @click="loadMore"
      >
        {{ isLoadingMore ? "Loading…" : "Load more" }}
      </button>
    </div>

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
