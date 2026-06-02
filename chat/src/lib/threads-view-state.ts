import { computed, ref, watch, type Ref } from "vue";
import type { ChannelSummary } from "@/lib/chat-types";
import type { WasmThreadEntry, WasmThreadsPage } from "@/lib/xmpp/wasm-types";
import {
  activeSinceIso,
  decodeThreadsFilterState,
  encodeThreadsFilterState,
  type ThreadsActiveWindow,
  type ThreadsSort,
  type ThreadsStatusFilter,
} from "@/lib/threads-view-filters";

const THREADS_PAGE_SIZE = 50;

export interface ThreadsViewClient {
  fetchThreads(opts?: {
    pageSize?: number;
    afterCursor?: string;
    status?: ThreadsStatusFilter;
    activeSince?: string;
    channel?: string;
    search?: string;
    sort?: ThreadsSort;
  }): Promise<WasmThreadsPage>;
  markInboxRead(partnerJid: string, threadId?: string): Promise<void>;
}

interface ThreadsUrlStateAdapter {
  readSearch(): string;
  replaceSearch(encoded: string): void;
}

export function useThreadsListPanelState(options: {
  xmppClient: Readonly<Ref<ThreadsViewClient | null>>;
  channels: Readonly<Ref<readonly ChannelSummary[]>>;
  urlState?: ThreadsUrlStateAdapter;
  now?: () => Date;
}) {
  const initialFilters = decodeThreadsFilterState(options.urlState?.readSearch() ?? "");
  const now = options.now ?? (() => new Date());

  const status = ref<ThreadsStatusFilter>(initialFilters.status);
  const active = ref<ThreadsActiveWindow>(initialFilters.active);
  const channel = ref(initialFilters.channel);
  const search = ref(initialFilters.query);
  const sort = ref<ThreadsSort>(initialFilters.sort);

  const entries = ref<WasmThreadEntry[]>([]);
  const total = ref(0);
  const unreadThreads = ref(0);
  const nextCursor = ref<string | null>(null);
  const loading = ref(true);
  const loadingMore = ref(false);
  const error = ref<string | null>(null);
  const markingReadKey = ref<string | null>(null);
  let requestSerial = 0;

  const filterState = computed(() => ({
    status: status.value,
    active: active.value,
    channel: channel.value,
    query: search.value,
    sort: sort.value,
  }));

  const selectableChannels = computed(() =>
    options.channels.value
      .filter((item) => !!item.jid)
      .slice()
      .sort((left, right) => left.name.localeCompare(right.name)),
  );

  const selectedChannelJid = computed(() => {
    if (channel.value === "all") return undefined;
    return selectableChannels.value.find((item) => item.id === channel.value)?.jid;
  });
  const channelFilterResolved = computed(
    () => channel.value === "all" || !!selectedChannelJid.value,
  );
  const channelFilterMessage = computed(() => {
    if (channel.value === "all" || selectedChannelJid.value) return "";
    return selectableChannels.value.length === 0
      ? "Waiting for channel metadata before filtering threads."
      : "Selected channel is no longer available.";
  });
  const channelFilterOptionLabel = computed(() => {
    if (channel.value === "all" || selectedChannelJid.value) return "";
    return selectableChannels.value.length === 0
      ? "Loading selected channel"
      : "Selected channel unavailable";
  });

  const requestState = computed(() => ({
    ...filterState.value,
    channelJid: selectedChannelJid.value ?? "",
  }));

  const unread = computed(() => entries.value.filter((entry) => entry.has_unread));
  const following = computed(() => entries.value.filter((entry) => !entry.has_unread));
  const sections = computed(() => {
    if (status.value === "unread") {
      return [{ id: "unread", label: "Unread", entries: unread.value }];
    }
    if (status.value === "following") {
      return [{ id: "following", label: "Following", entries: following.value }];
    }
    return [{ id: "all", label: "All", entries: entries.value }];
  });
  const hasEntries = computed(() => entries.value.length > 0);
  const resultSummary = computed(() => {
    if (loading.value && !hasEntries.value) return "";
    const threadLabel = total.value === 1 ? "thread" : "threads";
    if (unreadThreads.value === 0) return `${total.value} ${threadLabel}`;
    return `${total.value} ${threadLabel} · ${unreadThreads.value} unread`;
  });

  function threadKey(entry: WasmThreadEntry): string {
    return `${entry.channel}|${entry.thread_id}`;
  }

  function updateUrl() {
    options.urlState?.replaceSearch(encodeThreadsFilterState(filterState.value));
  }

  async function fetchThreadsPage(append: boolean) {
    const client = options.xmppClient.value;
    if (!client) {
      loading.value = false;
      loadingMore.value = false;
      return;
    }
    if (!channelFilterResolved.value) {
      if (!append) {
        entries.value = [];
        total.value = 0;
        unreadThreads.value = 0;
        nextCursor.value = null;
      }
      loading.value = false;
      loadingMore.value = false;
      return;
    }
    if (append && !nextCursor.value) return;

    const serial = ++requestSerial;
    if (append) loadingMore.value = true;
    else {
      loading.value = true;
      entries.value = [];
      nextCursor.value = null;
    }
    error.value = null;

    try {
      const activeSince = activeSinceIso(active.value, now());
      const page = await client.fetchThreads({
        pageSize: THREADS_PAGE_SIZE,
        ...(append && nextCursor.value ? { afterCursor: nextCursor.value } : {}),
        status: status.value,
        ...(activeSince ? { activeSince } : {}),
        ...(selectedChannelJid.value ? { channel: selectedChannelJid.value } : {}),
        ...(search.value.trim() ? { search: search.value.trim() } : {}),
        sort: sort.value,
      });
      if (serial !== requestSerial) return;
      entries.value = append ? [...entries.value, ...page.entries] : page.entries;
      total.value = page.total;
      unreadThreads.value = page.unread_threads;
      nextCursor.value = page.next_cursor ?? null;
    } catch (err) {
      if (serial !== requestSerial) return;
      error.value = err instanceof Error ? err.message : String(err);
    } finally {
      if (serial === requestSerial) {
        loading.value = false;
        loadingMore.value = false;
      }
    }
  }

  async function markThreadRead(entry: WasmThreadEntry) {
    const client = options.xmppClient.value;
    if (!client) return;
    markingReadKey.value = threadKey(entry);
    try {
      await client.markInboxRead(entry.channel, entry.thread_id);
      await fetchThreadsPage(false);
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
    } finally {
      markingReadKey.value = null;
    }
  }

  watch(requestState, () => {
    updateUrl();
    void fetchThreadsPage(false);
  });

  watch(options.xmppClient, (client) => {
    if (client) void fetchThreadsPage(false);
  });

  return {
    active,
    channel,
    channelFilterMessage,
    channelFilterOptionLabel,
    entries,
    error,
    fetchThreadsPage,
    filterState,
    following,
    hasEntries,
    loading,
    loadingMore,
    markThreadRead,
    markingReadKey,
    nextCursor,
    resultSummary,
    search,
    sections,
    selectableChannels,
    sort,
    status,
    threadKey,
    total,
    unread,
    unreadThreads,
  };
}
