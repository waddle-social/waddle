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

  watch(memberQuery, (query) => {
    if (searchTimer) {
      clearTimeout(searchTimer);
      searchTimer = null;
    }

    const trimmed = query.trim();
    if (!trimmed || !api.value || !canManageMembers.value || !activeWaddleId.value) {
      memberSearchResults.value = [];
      isSearchingUsers.value = false;
      return;
    }

    searchTimer = setTimeout(async () => {
      isSearchingUsers.value = true;
      try {
        const res = await api.value!.searchUsers(trimmed);
        const existingIds = new Set(members.value.map((m) => m.user_id));
        memberSearchResults.value = res.users.filter((u) => !existingIds.has(u.id));
      } catch (e) {
        actionError.value = normalizeError(e);
      } finally {
        isSearchingUsers.value = false;
      }
    }, 220);
  });

  async function addMember(userId: string) {
    if (!api.value || !activeWaddleId.value) return;

    clearActionError();
    try {
      await api.value.addMember(activeWaddleId.value, {
        user_id: userId,
        role: newMemberRole.value,
      });
      memberQuery.value = "";
      memberSearchResults.value = [];
      await reloadStructure(activeWaddleId.value, activeChannelId.value);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function updateMemberRole(member: MemberSummary, role: EditableRole) {
    if (!api.value || !activeWaddleId.value || member.role === "owner") return;

    clearActionError();
    try {
      await api.value.updateMemberRole(activeWaddleId.value, member.user_id, role);
      await reloadStructure(activeWaddleId.value, activeChannelId.value);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function removeMember(member: MemberSummary) {
    if (!api.value || !activeWaddleId.value || member.role === "owner") return;

    clearActionError();
    try {
      await api.value.removeMember(activeWaddleId.value, member.user_id);
      await reloadStructure(activeWaddleId.value, activeChannelId.value);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  function clearSearch() {
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
