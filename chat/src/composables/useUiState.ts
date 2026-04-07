import { ref } from "vue";
import type { AdminTab } from "@/lib/chat-ui";

export function useUiState() {
  const adminTab = ref<AdminTab>("rooms");
  const showMobileNav = ref(false);
  const showMobileDetails = ref(false);
  const showCreateWaddle = ref(false);
  const showCreateChannel = ref(false);
  const showEditChannel = ref(false);
  const showWaddleSettings = ref(false);
  const actionError = ref("");

  function clearActionError() {
    actionError.value = "";
  }

  function normalizeError(value: unknown) {
    return value instanceof Error ? value.message : "Something went wrong.";
  }

  return {
    adminTab,
    showMobileNav,
    showMobileDetails,
    showCreateWaddle,
    showCreateChannel,
    showEditChannel,
    showWaddleSettings,
    actionError,
    clearActionError,
    normalizeError,
  };
}
