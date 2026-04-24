import { ref, watch, type Ref } from "vue";
import type { MemberSummary, UserSearchResult } from "@/lib/chat-types";
import type { EditableRole } from "@/lib/chat-ui";

export function useMembers(
  _api: Ref<null>,
  activeSpaceId: Ref<string | null>,
  _activeChannelId: Ref<string | null>,
  _members: Ref<MemberSummary[]>,
  canManageMembers: Ref<boolean>,
  _normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
  _reloadStructure: (channelId?: string | null) => Promise<string | null>,
) {
  const memberQuery = ref("");
  const newMemberRole = ref<EditableRole>("member");
  const memberSearchResults = ref<UserSearchResult[]>([]);
  const isSearchingUsers = ref(false);

  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  let searchRequestId = 0;

  watch(memberQuery, (query) => {
    if (searchTimer) {
      clearTimeout(searchTimer);
      searchTimer = null;
    }

    const trimmed = query.trim();
    if (!trimmed || !canManageMembers.value || !activeSpaceId.value) {
      searchRequestId++;
      memberSearchResults.value = [];
      isSearchingUsers.value = false;
      return;
    }

    searchTimer = setTimeout(async () => {
      const requestId = ++searchRequestId;
      const spaceId = activeSpaceId.value;
      if (!spaceId || !canManageMembers.value) return;

      isSearchingUsers.value = true;
      if (
        requestId === searchRequestId &&
        activeSpaceId.value === spaceId &&
        memberQuery.value.trim() === trimmed
      ) {
        memberSearchResults.value = [];
      }
      if (requestId === searchRequestId) {
        isSearchingUsers.value = false;
      }
    }, 220);
  });

  async function addMember(_userId: string) {
    clearActionError();
    actionError.value = "Member management is not available in the XMPP-only client yet.";
  }

  async function updateMemberRole(_member: MemberSummary, _role: EditableRole) {
    clearActionError();
    actionError.value = "Member role management is not available in the XMPP-only client yet.";
  }

  async function removeMember(_member: MemberSummary) {
    clearActionError();
    actionError.value = "Member removal is not available in the XMPP-only client yet.";
  }

  function clearSearch() {
    searchRequestId++;
    memberQuery.value = "";
    memberSearchResults.value = [];
    newMemberRole.value = "member";
    if (searchTimer) {
      clearTimeout(searchTimer);
      searchTimer = null;
    }
  }

  return {
    memberQuery,
    newMemberRole,
    memberSearchResults,
    isSearchingUsers,
    addMember,
    updateMemberRole,
    removeMember,
    clearSearch,
  };
}
