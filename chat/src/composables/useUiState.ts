import { ref } from "vue";
import type { AdminTab } from "@/lib/chat-ui";

export function useUiState() {
  const activePage = ref<"chat" | "settings">("chat");
  const adminTab = ref<AdminTab>("rooms");
  const sidebarMode = ref<"channels" | "dms">("channels");
  const showMobileNav = ref(false);
  const showMobileDetails = ref(false);
  const showCreateWaddle = ref(false);
  const showBrowsePublicWaddles = ref(false);
  const showCreateChannel = ref(false);
  const showEditChannel = ref(false);
  const showWaddleSettings = ref(false);
  const showMembers = ref(false);
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
    showMobileNav,
    showMobileDetails,
    showCreateWaddle,
    showBrowsePublicWaddles,
    showCreateChannel,
    showEditChannel,
    showWaddleSettings,
    showMembers,
    confirmDeleteWaddle,
    showNewDm,
    confirmDeleteChannel,
    confirmRemoveMember,
    actionError,
    clearActionError,
    normalizeError,
  };
}
