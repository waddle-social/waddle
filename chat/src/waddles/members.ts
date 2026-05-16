import { ref, watch, type Ref } from "vue";
import type { MemberSummary, UserSearchResult } from "@/lib/chat-types";
import type { EditableAffiliation } from "@/lib/chat-ui";
import type { BrowserXmppClient } from "@/lib/xmpp-client";

export function useWaddleMembers(
  xmppClient: Ref<BrowserXmppClient | null>,
  activeSpaceId: Ref<string | null>,
  activeChannelId: Ref<string | null>,
  members: Ref<MemberSummary[]>,
  canManageMembers: Ref<boolean>,
  _normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
  _reloadStructure: (channelId?: string | null) => Promise<string | null>,
) {
  const memberQuery = ref("");
  const newMemberAffiliation = ref<EditableAffiliation>("member");
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
      if (!spaceId || !canManageMembers.value || !xmppClient.value) return;

      isSearchingUsers.value = true;
      try {
        const results = await xmppClient.value.searchUsers(trimmed);
        if (
          requestId === searchRequestId &&
          activeSpaceId.value === spaceId &&
          memberQuery.value.trim() === trimmed
        ) {
          const existing = new Set(members.value.map((member) => member.jid));
          memberSearchResults.value = results.filter((user) => !existing.has(user.jid));
        }
      } catch (e) {
        if (requestId === searchRequestId) actionError.value = _normalizeError(e);
      } finally {
        if (requestId === searchRequestId) {
        isSearchingUsers.value = false;
        }
      }
    }, 220);
  });

  async function addMember(userId: string) {
    clearActionError();
    if (!xmppClient.value || !activeChannelId.value) return;
    await xmppClient.value.setRoomAffiliation(activeChannelId.value, userId, newMemberAffiliation.value);
    members.value = await xmppClient.value.listRoomMembers(activeChannelId.value);
    clearSearch();
  }

  async function updateMemberAffiliation(member: MemberSummary, affiliation: EditableAffiliation) {
    clearActionError();
    if (!xmppClient.value || !activeChannelId.value) return;
    await xmppClient.value.setRoomAffiliation(activeChannelId.value, member.jid, affiliation);
    members.value = await xmppClient.value.listRoomMembers(activeChannelId.value);
  }

  async function removeMember(member: MemberSummary) {
    clearActionError();
    if (!xmppClient.value || !activeChannelId.value) return;
    await xmppClient.value.setRoomAffiliation(activeChannelId.value, member.jid, "none");
    members.value = await xmppClient.value.listRoomMembers(activeChannelId.value);
  }

  function clearSearch() {
    searchRequestId++;
    memberQuery.value = "";
    memberSearchResults.value = [];
    newMemberAffiliation.value = "member";
    if (searchTimer) {
      clearTimeout(searchTimer);
      searchTimer = null;
    }
  }

  return {
    memberQuery,
    newMemberAffiliation,
    memberSearchResults,
    isSearchingUsers,
    addMember,
    updateMemberAffiliation,
    removeMember,
    clearSearch,
  };
}
