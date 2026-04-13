import { ref, watch, type Ref } from "vue";
import type { WaddleApi, MemberSummary, UserSearchResult } from "@/lib/waddle-api";
import type { EditableRole } from "@/lib/chat-ui";

export function useMembers(
  api: Ref<WaddleApi | null>,
  activeWaddleId: Ref<string | null>,
  activeChannelId: Ref<string | null>,
  members: Ref<MemberSummary[]>,
  canManageMembers: Ref<boolean>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
  reloadStructure: (waddleId: string, channelId?: string | null) => Promise<string | null>,
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
    if (!trimmed || !api.value || !canManageMembers.value || !activeWaddleId.value) {
      searchRequestId++;
      memberSearchResults.value = [];
      isSearchingUsers.value = false;
      return;
    }

    searchTimer = setTimeout(async () => {
      const requestId = ++searchRequestId;
      const client = api.value;
      const waddleId = activeWaddleId.value;
      if (!client || !waddleId || !canManageMembers.value) return;

      isSearchingUsers.value = true;
      try {
        const res = await client.searchUsers(trimmed);
        if (
          requestId !== searchRequestId ||
          activeWaddleId.value !== waddleId ||
          memberQuery.value.trim() !== trimmed
        )
          return;

        const existingIds = new Set(members.value.map((m) => m.user_id));
        memberSearchResults.value = res.users.filter((u) => !existingIds.has(u.id));
      } catch (e) {
        if (requestId === searchRequestId) {
          actionError.value = normalizeError(e);
        }
      } finally {
        if (requestId === searchRequestId) {
          isSearchingUsers.value = false;
        }
      }
    }, 220);
  });

  async function addMember(userId: string) {
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!api.value || !waddleId) return;

    clearActionError();
    try {
      await api.value.addMember(waddleId, {
        user_id: userId,
        role: newMemberRole.value,
      });
      memberQuery.value = "";
      memberSearchResults.value = [];
      await reloadStructure(waddleId, channelId);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function updateMemberRole(member: MemberSummary, role: EditableRole) {
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!api.value || !waddleId || member.role === "owner") return;

    clearActionError();
    try {
      await api.value.updateMemberRole(waddleId, member.user_id, role);
      await reloadStructure(waddleId, channelId);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function removeMember(member: MemberSummary) {
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!api.value || !waddleId || member.role === "owner") return;

    clearActionError();
    try {
      await api.value.removeMember(waddleId, member.user_id);
      await reloadStructure(waddleId, channelId);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
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
