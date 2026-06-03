export type ThreadsStatusFilter = "all" | "unread" | "following";
export type ThreadsActiveWindow = "7d" | "14d" | "30d" | "all";
export type ThreadsSort = "recent" | "unread" | "replies";

interface ThreadsFilterState {
  status: ThreadsStatusFilter;
  active: ThreadsActiveWindow;
  channel: string;
  query: string;
  sort: ThreadsSort;
}

const VALID_STATUS = new Set<ThreadsStatusFilter>(["all", "unread", "following"]);
const VALID_ACTIVE = new Set<ThreadsActiveWindow>(["7d", "14d", "30d", "all"]);
const VALID_SORT = new Set<ThreadsSort>(["recent", "unread", "replies"]);

const DEFAULT_THREADS_FILTERS: ThreadsFilterState = {
  status: "unread",
  active: "7d",
  channel: "all",
  query: "",
  sort: "recent",
};

function paramOrDefault<T extends string>(
  params: URLSearchParams,
  key: string,
  allowed: Set<T>,
  fallback: T,
): T {
  const value = params.get(key);
  return value && allowed.has(value as T) ? (value as T) : fallback;
}

export function decodeThreadsFilterState(search: string | URLSearchParams): ThreadsFilterState {
  const params = typeof search === "string" ? new URLSearchParams(search) : search;
  const channel = params.get("channel")?.trim() || DEFAULT_THREADS_FILTERS.channel;
  return {
    status: paramOrDefault(params, "status", VALID_STATUS, DEFAULT_THREADS_FILTERS.status),
    active: paramOrDefault(params, "active", VALID_ACTIVE, DEFAULT_THREADS_FILTERS.active),
    channel,
    query: params.get("q")?.trim() ?? DEFAULT_THREADS_FILTERS.query,
    sort: paramOrDefault(params, "sort", VALID_SORT, DEFAULT_THREADS_FILTERS.sort),
  };
}

export function encodeThreadsFilterState(filters: ThreadsFilterState): string {
  const params = new URLSearchParams();
  if (filters.status !== DEFAULT_THREADS_FILTERS.status) params.set("status", filters.status);
  if (filters.active !== DEFAULT_THREADS_FILTERS.active) params.set("active", filters.active);
  if (filters.channel !== DEFAULT_THREADS_FILTERS.channel) params.set("channel", filters.channel);
  const query = filters.query.trim();
  if (query) params.set("q", query);
  if (filters.sort !== DEFAULT_THREADS_FILTERS.sort) params.set("sort", filters.sort);
  return params.toString();
}

export function activeSinceIso(window: ThreadsActiveWindow, now = new Date()): string | undefined {
  if (window === "all") return undefined;
  const days = Number.parseInt(window, 10);
  if (!Number.isFinite(days)) return undefined;
  const startOfToday = new Date(Date.UTC(
    now.getUTCFullYear(),
    now.getUTCMonth(),
    now.getUTCDate(),
  ));
  startOfToday.setUTCDate(startOfToday.getUTCDate() - days);
  return startOfToday.toISOString();
}

function normalizeThreadPreview(value: string | undefined): string | undefined {
  const text = value?.trim();
  if (!text) return undefined;
  if (/^https?:\/\/\S+\.(gif|webp|png|jpe?g)(\?\S*)?$/i.test(text)) {
    return "Media attachment";
  }
  if (/^https?:\/\/media\d*\.giphy\.com\//i.test(text)) {
    return "GIF attachment";
  }
  return text.replace(/^>\s*/, "").trim();
}

export function threadDisplayTitle(entry: {
  thread_title?: string;
  preview?: string;
}): string {
  return normalizeThreadPreview(entry.thread_title)
    ?? normalizeThreadPreview(entry.preview)
    ?? "Thread";
}
