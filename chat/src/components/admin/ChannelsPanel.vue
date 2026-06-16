<script setup lang="ts">
// Admin V2 — Channels panel. Mirrors `SpacesPanel.vue` plus a row of
// space-filter chips above the search input.
import { computed, onMounted, ref, watch } from "vue";
import { Plus, Search } from "lucide-vue-next";
import type { BrowserXmppClient } from "@/lib/xmpp";
import type {
  WasmAdminChannelListEntry,
  WasmAdminChannelsListResult,
  WasmAdminSpaceListEntry,
} from "@/lib/xmpp";
import ChannelCreateDialog from "@/components/admin/ChannelCreateDialog.vue";
import ChannelDetailDrawer from "@/components/admin/ChannelDetailDrawer.vue";

const props = defineProps<{
  xmppClient: BrowserXmppClient | null;
}>();

const SEARCH_DEBOUNCE_MS = 200;
const PAGE_SIZE = 50;
const SPACE_OPTIONS_PAGE_SIZE = 200;
type SpaceOption = Pick<WasmAdminSpaceListEntry, "space_jid" | "space_node" | "name">;

const entries = ref<WasmAdminChannelListEntry[]>([]);
const cursor = ref<string | null>(null);
const isLoading = ref(false);
const isLoadingMore = ref(false);
const errorMessage = ref("");
const prefix = ref("");
const debouncedPrefix = ref("");
const spaceFilter = ref<SpaceOption | null>(null);
const spaces = ref<SpaceOption[]>([]);
const activePrefix = ref("");
const activeSpaceJid = ref<string | null>(null);
const activeSpaceNode = ref<string | null>(null);
const showCreate = ref(false);
const isSubmitting = ref(false);
const selected = ref<WasmAdminChannelListEntry | null>(null);
let debounceTimer: ReturnType<typeof setTimeout> | null = null;
let requestId = 0;

async function loadSpaces() {
  if (!props.xmppClient) return;
  try {
    const loaded: SpaceOption[] = [];
    const seenCursors = new Set<string>();
    let afterCursor: string | null = null;
    do {
      const page = await props.xmppClient.adminSpacesList({
        pageSize: SPACE_OPTIONS_PAGE_SIZE,
        afterCursor,
      });
      loaded.push(
        ...page.entries.map((space) => ({
          space_jid: space.space_jid,
          space_node: space.space_node,
          name: space.name,
        })),
      );
      const nextCursor = page.next_cursor ?? null;
      if (nextCursor && seenCursors.has(nextCursor)) break;
      if (nextCursor) seenCursors.add(nextCursor);
      afterCursor = nextCursor;
    } while (afterCursor);
    spaces.value = loaded;
    if (
      spaceFilter.value &&
      !loaded.some((space) => space.space_node === spaceFilter.value?.space_node)
    ) {
      spaceFilter.value = null;
    }
  } catch {
    /* ignored — the filter row gracefully degrades to "All" */
  }
}

async function fetchFirstPage(currentPrefix: string, currentSpace: SpaceOption | null): Promise<void> {
  if (!props.xmppClient) return;
  const localRequestId = ++requestId;
  isLoading.value = true;
  isLoadingMore.value = false;
  cursor.value = null;
  errorMessage.value = "";
  try {
    const page: WasmAdminChannelsListResult = await props.xmppClient.adminChannelsList({
      spaceJid: currentSpace?.space_jid ?? null,
      spaceNode: currentSpace?.space_node ?? null,
      prefix: currentPrefix || null,
      pageSize: PAGE_SIZE,
    });
    if (requestId !== localRequestId) return;
    activePrefix.value = currentPrefix;
    activeSpaceJid.value = currentSpace?.space_jid ?? null;
    activeSpaceNode.value = currentSpace?.space_node ?? null;
    entries.value = page.entries;
    cursor.value = page.next_cursor ?? null;
  } catch (err: unknown) {
    if (requestId !== localRequestId) return;
    errorMessage.value = err instanceof Error ? err.message : "Failed to load channels.";
  } finally {
    if (requestId === localRequestId) isLoading.value = false;
  }
}

async function loadMore(): Promise<void> {
  if (!props.xmppClient || !cursor.value || isLoading.value || isLoadingMore.value) return;
  const localRequestId = requestId;
  const afterCursor = cursor.value;
  const currentPrefix = activePrefix.value;
  const currentSpaceJid = activeSpaceJid.value;
  const currentSpaceNode = activeSpaceNode.value;
  isLoadingMore.value = true;
  try {
    const page = await props.xmppClient.adminChannelsList({
      spaceJid: currentSpaceJid,
      spaceNode: currentSpaceNode,
      prefix: currentPrefix || null,
      pageSize: PAGE_SIZE,
      afterCursor,
    });
    if (
      requestId !== localRequestId ||
      cursor.value !== afterCursor ||
      activePrefix.value !== currentPrefix ||
      activeSpaceJid.value !== currentSpaceJid ||
      activeSpaceNode.value !== currentSpaceNode
    ) {
      return;
    }
    entries.value = entries.value.concat(page.entries);
    cursor.value = page.next_cursor ?? null;
  } catch (err: unknown) {
    if (requestId !== localRequestId) return;
    errorMessage.value = err instanceof Error ? err.message : "Failed to load more channels.";
  } finally {
    isLoadingMore.value = false;
  }
}

async function onCreate(payload: { name: string; topic?: string | null; spaceJid?: string | null; spaceNode?: string | null; isPublic?: boolean | null }) {
  if (!props.xmppClient) return;
  isSubmitting.value = true;
  try {
    await props.xmppClient.adminChannelsCreate(payload);
    showCreate.value = false;
    await fetchFirstPage(prefix.value, spaceFilter.value);
  } catch (err: unknown) {
    errorMessage.value = err instanceof Error ? err.message : "Failed to create channel.";
  } finally {
    isSubmitting.value = false;
  }
}

function openDetail(entry: WasmAdminChannelListEntry) {
  selected.value = entry;
}
function closeDetail() {
  selected.value = null;
}
async function onDetailChanged() {
  await fetchFirstPage(prefix.value, spaceFilter.value);
}
async function onDetailDeleted() {
  selected.value = null;
  await fetchFirstPage(prefix.value, spaceFilter.value);
}

watch(prefix, (value) => {
  if (debounceTimer) clearTimeout(debounceTimer);
  debounceTimer = setTimeout(() => {
    debouncedPrefix.value = value;
  }, SEARCH_DEBOUNCE_MS);
});

watch(debouncedPrefix, (value) => {
  void fetchFirstPage(value, spaceFilter.value);
});

watch(spaceFilter, (value) => {
  void fetchFirstPage(prefix.value, value);
});

onMounted(() => {
  void loadSpaces();
  void fetchFirstPage("", null);
});

const hasMore = computed(() => cursor.value !== null);
const showEmptyState = computed(
  () => !isLoading.value && entries.value.length === 0 && !errorMessage.value,
);
const dialogSpaces = computed(() =>
  spaces.value.map((space) => ({
    space_jid: space.space_jid,
    space_node: space.space_node,
    name: space.name,
  })),
);
</script>

<template>
  <section class="flex flex-col gap-4 p-4 max-w-4xl mx-auto w-full" aria-labelledby="channels-panel-heading">
    <header class="flex flex-col gap-3">
      <div class="flex items-center justify-between gap-3">
        <div class="flex flex-col gap-1">
          <h2 id="channels-panel-heading" class="type-pane-title">Channels</h2>
          <p class="type-caption text-muted-foreground max-w-prose">
            All MUC rooms on this server. Click a row to edit
            config, manage affiliations, or kick occupants.
          </p>
        </div>
        <button
          type="button"
          class="chat-action-button chat-action-button--primary type-action"
          aria-label="Create channel"
          @click="showCreate = true"
        >
          <Plus class="w-4 h-4" />
          <span>New channel</span>
        </button>
      </div>

      <!-- Space filter chips -->
      <div v-if="spaces.length > 0" class="flex flex-wrap items-center gap-1.5" role="group" aria-label="Filter by space">
        <button
          type="button"
          class="rounded-full border px-2.5 py-0.5 type-caption transition-colors"
          :class="spaceFilter === null ? 'border-primary/40 bg-primary/10 text-primary' : 'border-border bg-card hover:bg-muted'"
          @click="spaceFilter = null"
        >
          All
        </button>
        <button
          v-for="s in spaces"
          :key="s.space_node"
          type="button"
          class="rounded-full border px-2.5 py-0.5 type-caption transition-colors"
          :class="spaceFilter?.space_node === s.space_node ? 'border-primary/40 bg-primary/10 text-primary' : 'border-border bg-card hover:bg-muted'"
          @click="spaceFilter = s"
        >
          {{ s.name }}
        </button>
      </div>

      <div class="flex items-center gap-2 chat-field-control max-w-md">
        <Search class="w-4 h-4 text-muted-foreground" aria-hidden="true" />
        <input
          v-model="prefix"
          type="search"
          autocomplete="off"
          placeholder="Search channels"
          aria-label="Filter channels by prefix"
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
      Loading channels…
    </div>

    <ul v-else-if="entries.length > 0" class="flex flex-col gap-2" role="list">
      <li v-for="entry in entries" :key="entry.channel_jid">
        <button
          type="button"
          class="w-full flex items-center justify-between gap-3 rounded-lg border border-border bg-card px-3 py-2.5 text-left hover:bg-muted transition-colors"
          :aria-label="`Open ${entry.name}`"
          @click="openDetail(entry)"
        >
          <div class="flex flex-col gap-0.5 min-w-0 flex-1">
            <div class="flex items-center gap-2 min-w-0">
              <span class="type-control truncate">{{ entry.name }}</span>
              <span v-if="!entry.is_public" class="type-caption rounded-full bg-muted px-2 py-0.5 flex-shrink-0">Private</span>
            </div>
            <span class="type-caption text-muted-foreground truncate font-mono">{{ entry.channel_jid }}</span>
            <span v-if="entry.topic" class="type-caption text-muted-foreground truncate">{{ entry.topic }}</span>
          </div>
          <div class="flex items-center gap-2 flex-shrink-0">
            <span class="type-caption rounded-full bg-muted px-2 py-0.5">{{ entry.occupant_count }} live</span>
            <span class="type-caption rounded-full bg-muted px-2 py-0.5">{{ entry.member_count }} mem</span>
          </div>
        </button>
      </li>
    </ul>

    <div v-else-if="showEmptyState" class="py-12 text-center type-caption text-muted-foreground" role="status">
      <p v-if="prefix">No channels match “{{ prefix }}”.</p>
      <p v-else-if="spaceFilter">No channels in this space yet.</p>
      <p v-else>No channels yet. Create one to get started.</p>
    </div>

    <div v-if="hasMore" class="flex justify-center pt-2">
      <button
        type="button"
        class="chat-action-button chat-action-button--secondary type-control"
        :disabled="isLoadingMore || isLoading"
        @click="loadMore"
      >
        {{ isLoadingMore ? "Loading…" : "Load more" }}
      </button>
    </div>

    <ChannelCreateDialog
      v-model:open="showCreate"
      :is-submitting="isSubmitting"
      :spaces="dialogSpaces"
      @submit="onCreate"
    />
    <ChannelDetailDrawer
      v-if="selected"
      :open="!!selected"
      :xmpp-client="xmppClient"
      :channel="selected"
      @close="closeDetail"
      @changed="onDetailChanged"
      @deleted="onDetailDeleted"
    />
  </section>
</template>
