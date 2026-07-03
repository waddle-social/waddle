import { ref } from "vue";
import type { AdminTab } from "@/lib/chat-ui";

/** Community pseudo-channels surfaced at the top of the sidebar.
 * Each renders its own content pane when active, rather than the
 * channel timeline. */
export type CommunitySurface = "feed" | "events";
export type FeedSurfaceFilter = "all" | "pep" | "stories" | "posts";
export type FeedSurfaceComposerMode = "post" | "story";

export type ChatShellState = ReturnType<typeof useChatShellState>;

export function useChatShellState() {
  const activePage = ref<"dashboard" | "chat" | "settings" | "admin" | "threads" | "unread">("dashboard");
  const adminTab = ref<AdminTab>("rooms");
  const sidebarMode = ref<"channels" | "dms">("channels");
  /** Which community pseudo-channel is currently active (if any).
   * When non-null the content area renders the corresponding
   * surface in place of the channel timeline. */
  const activeCommunitySurface = ref<CommunitySurface | null>(null);
  const feedDefaultFilter = ref<FeedSurfaceFilter>("all");
  const feedDefaultComposerMode = ref<FeedSurfaceComposerMode>("post");
  const collapsedSpaceGroupIds = ref<Set<string>>(new Set());
  const createChannelContextSpaceId = ref<string | null>(null);
  const showMobileNav = ref(false);
  const showMobileDetails = ref(false);
  const showCreateChannel = ref(false);
  const showEditChannel = ref(false);
  const showWaddleSettings = ref(false);
  const showMembers = ref(false);
  /** #414: pinned-messages right-rail panel toggle. Synced with `?pinned=1`. */
  const showPinnedPanel = ref(false);
  const confirmDeleteWaddle = ref(false);
  const confirmDeleteChannel = ref(false);
  const showNewDm = ref(false);
  const showNewGroupDm = ref(false);
  const groupDmSeedPeerJid = ref<string | null>(null);
  const confirmRemoveMember = ref<string | null>(null);
  const actionError = ref("");

  function clearActionError() {
    actionError.value = "";
  }

  function normalizeError(value: unknown) {
    return value instanceof Error ? value.message : "Something went wrong.";
  }

  return {
    activePage,
    adminTab,
    sidebarMode,
    activeCommunitySurface,
    feedDefaultFilter,
    feedDefaultComposerMode,
    collapsedSpaceGroupIds,
    createChannelContextSpaceId,
    showMobileNav,
    showMobileDetails,
    showCreateChannel,
    showEditChannel,
    showWaddleSettings,
    showMembers,
    showPinnedPanel,
    confirmDeleteWaddle,
    showNewDm,
    showNewGroupDm,
    groupDmSeedPeerJid,
    confirmDeleteChannel,
    confirmRemoveMember,
    actionError,
    clearActionError,
    normalizeError,
  };
}
