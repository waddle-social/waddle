<script setup lang="ts">
import AppSelect from "@/components/ui/AppSelect.vue";
// Admin V2 — Channels panel. Mirrors `SpacesPanel.vue` plus a row of
// space-filter chips above the search input.
import { computed, onMounted, ref, watch } from "vue";
import { Plus } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { btn } from "styled-system/recipes";
import { adminClasses as a } from "@/components/admin/admin-classes";
import type { BrowserXmppClient } from "@/lib/xmpp";
import type {
  WasmAdminChannelListEntry,
  WasmAdminChannelType,
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

async function onCreate(payload: { name: string; topic?: string | null; channelType?: WasmAdminChannelType | null; spaceJid?: string | null; spaceNode?: string | null; isPublic?: boolean | null; membersOnly?: boolean | null }) {
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
function channelTypeLabel(type: WasmAdminChannelType): string {
  switch (type) {
    case "announcement":
      return "Announcement";
    case "forum":
      return "Forum";
    case "group-dm":
      return "Group DM";
    case "text":
    default:
      return "Text";
  }
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

const panelClass = css({ display: "flex", flexDirection: "column", gap: "4" });
const newClass = btn({ variant: "primary" });
const filtersClass = css({ display: "flex", gap: "2", maxWidth: "640px", _narrow: { flexDirection: "column" } });
const spaceSelectClass = cx(a.input, css({ md: { width: "220px", flex: "none" } }));

/** The space filter as a select value: the space node, or "" for all. */
const spaceFilterNode = computed({
  get: () => spaceFilter.value?.space_node ?? "",
  set: (node: string) => {
    spaceFilter.value = spaces.value.find((space) => space.space_node === node) ?? null;
  },
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
  <section :class="panelClass" aria-label="Channels">
    <Teleport defer to="#admin-header-actions">
      <button type="button" :class="newClass" aria-label="Create channel" @click="showCreate = true">
        <Plus aria-hidden="true" />
        <span>New channel</span>
      </button>
    </Teleport>
    <div :class="filtersClass">
      <label :class="a.srOnly" for="channels-prefix">Search channels</label>
      <input
        id="channels-prefix"
        v-model="prefix"
        :class="a.input"
        type="search"
        autocomplete="off"
        placeholder="Search channels"
        aria-label="Filter channels by prefix"
      />
      <AppSelect
        v-if="spaces.length > 0"
        v-model="spaceFilterNode"
        :options="[{ value: '', label: 'All spaces' }, ...spaces.map((s) => ({ value: s.space_node, label: s.name }))]"
        :class="spaceSelectClass"
        aria-label="Filter by space"
      />
    </div>

    <div v-if="errorMessage" :class="a.alert" role="alert">{{ errorMessage }}</div>

    <div v-if="isLoading && entries.length === 0" :class="a.status" role="status">
      Loading channels…
    </div>

    <ul v-else-if="entries.length > 0" :class="a.list" role="list">
      <li v-for="entry in entries" :key="entry.channel_jid">
        <button type="button" :class="a.row" :aria-label="`Open ${entry.name}`" @click="openDetail(entry)">
          <div :class="a.rowMain">
            <span :class="a.rowTitle">
              <b :class="a.truncate">{{ entry.name }}</b>
              <span :class="a.tag">{{ channelTypeLabel(entry.channel_type) }}</span>
              <span v-if="!entry.is_public" :class="a.tag">Hidden</span>
              <span v-if="entry.members_only" :class="a.tag">Members-only</span>
            </span>
            <span :class="a.jid">{{ entry.channel_jid }}</span>
            <span v-if="entry.topic" :class="a.sub">{{ entry.topic }}</span>
          </div>
          <span :class="a.meta">{{ entry.occupant_count }} local · {{ entry.member_count }} {{ entry.member_count === 1 ? "member" : "members" }}</span>
        </button>
      </li>
    </ul>

    <div v-else-if="showEmptyState" :class="a.status" role="status">
      <p v-if="prefix">No channels match “{{ prefix }}”.</p>
      <p v-else-if="spaceFilter">No channels in this space yet.</p>
      <p v-else>No channels yet. Create one to get started.</p>
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
