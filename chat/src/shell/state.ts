import { ref } from "vue";
import type { AdminTab } from "@/lib/chat-ui";

export function useChatShellState() {
  const activePage = ref<"dashboard" | "chat" | "settings">("dashboard");
  const adminTab = ref<AdminTab>("rooms");
  const sidebarMode = ref<"channels" | "dms">("channels");
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
    confirmDeleteChannel,
    confirmRemoveMember,
    actionError,
    clearActionError,
    normalizeError,
  };
}
