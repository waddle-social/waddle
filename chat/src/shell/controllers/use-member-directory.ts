import { computed, type ComputedRef, type Ref, ref, watch } from "vue";
import type { useChannelMessages } from "@/channels/messages";
import type { useDirectMessages } from "@/dms/messages";
import type { useWaddleDirectory, MemberLoadState } from "@/waddles/directory";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import type { MemberSummary } from "@/lib/chat-types";
import { avatarLookupCandidatesAcrossContexts, mentionAutocompleteCandidates, mergeMentionMembers } from "@/lib/mentions";

interface MemberDirectoryDeps {
  xmppClient: ComputedRef<BrowserXmppClient | null>;
  session: ComputedRef<WaddleSession | null>;
  waddles: ReturnType<typeof useWaddleDirectory>;
  messaging: ReturnType<typeof useChannelMessages>;
  dmMessaging: ReturnType<typeof useDirectMessages>;
  memberJidByNick: Ref<Record<string, string>>;
  mentionJidsByNickForSend: Ref<Record<string, string>>;
  selfDomain: ComputedRef<string>;
}

/**
 * Member roster, mention-autocomplete, and avatar bookkeeping for the
 * active channel: merges authoritative affiliation lists with live MUC
 * presence, keeps the outbound mention JID map in sync, and lazily
 * fetches avatars for every author the timelines surface.
 */
export function useMemberDirectory(deps: MemberDirectoryDeps) {
  const { xmppClient, session, waddles, messaging, dmMessaging, memberJidByNick, mentionJidsByNickForSend, selfDomain } = deps;

  const mergedMentionMembers = computed(() =>
    mergeMentionMembers({
      members: waddles.members.value,
      roomPresence: messaging.roomPresence.value,
      memberJidsByNick: memberJidByNick.value,
    }),
  );
  const authorJidByNick = computed(() => mergedMentionMembers.value.authorJidByNick);
  watch(authorJidByNick, (value) => {
    mentionJidsByNickForSend.value = value;
  }, { immediate: true });
  const fetchedAvatarUrlByJid = ref<Record<string, string | null>>({});
  const avatarFetchStateByJid = ref<Record<string, "pending" | "done">>({});
  const mentionCandidates = computed(() =>
    mentionAutocompleteCandidates(mergedMentionMembers.value.members).map((candidate) => {
      if (candidate.kind === "broadcast" || !candidate.jid) return candidate;
      return {
        ...candidate,
        avatar_url: fetchedAvatarUrlByJid.value[candidate.jid] ?? candidate.avatar_url,
      };
    }),
  );
  const mentionSourceDiagnostic = computed(() =>
    mergedMentionMembers.value.diagnostics.join(" "),
  );
  watch(mentionSourceDiagnostic, (detail) => {
    if (detail) console.warn(detail);
  });
  const memberAffiliationOrder = { owner: 0, admin: 1, member: 2, outcast: 3, none: 4 } as const;
  const displayedMembers = computed<MemberSummary[]>(() =>
    [...mergedMentionMembers.value.members].sort(
      (a, b) =>
        (memberAffiliationOrder[a.affiliation] ?? 4) - (memberAffiliationOrder[b.affiliation] ?? 4) ||
        a.username.localeCompare(b.username, undefined, { sensitivity: "base" }),
    ),
  );
  const authoritativeMemberJids = computed(() => new Set(waddles.members.value.map((member) => member.jid)));
  const inferredMemberJids = computed(() =>
    new Set(displayedMembers.value
      .filter((member) => !authoritativeMemberJids.value.has(member.jid))
      .map((member) => member.jid)),
  );
  const displayedMemberCount = computed<number | null>(() => {
    const count = displayedMembers.value.length;
    if (count > 0) return count;
    return waddles.memberLoadState.value === "ready" ? 0 : null;
  });
  const displayedMemberState = computed<MemberLoadState>(() => waddles.memberLoadState.value);
  const memberCountLabel = computed(() => {
    if (displayedMemberCount.value !== null) return String(displayedMemberCount.value);
    if (displayedMemberState.value === "loading") return "syncing";
    if (displayedMemberState.value === "unavailable") return "unavailable";
    return "0";
  });

  // RFC 363 PR 6: include DM peers in the iterate-and-pull avatar
  // candidate set. Without this, avatars in DM views never resolve
  // (the peer never appears in `messaging.messages` because that's
  // the channel-message timeline). After #435 (workspace roster
  // bridge) lands, push delivery covers both axes and this fallback
  // can be slimmed back down.
  const avatarCandidates = computed(() =>
    avatarLookupCandidatesAcrossContexts({
      channelMembers: mergedMentionMembers.value.members,
      channelMessages: messaging.messages.value,
      channelAuthorJidByNick: authorJidByNick.value,
      dmMessages: dmMessaging.messages.value,
      selfDomain: selfDomain.value,
    }),
  );
  const avatarUrlByAuthor = computed<Record<string, string | null>>(() => {
    const avatars: Record<string, string | null> = {};

    if (session.value) {
      avatars[session.value.username] = session.value.avatar_url;
    }

    for (const member of waddles.members.value) {
      const fetched = fetchedAvatarUrlByJid.value[member.jid];
      const avatar = fetched ?? member.avatar_url;
      if (!(member.username in avatars) || avatar) {
        avatars[member.username] = avatar;
      }
    }

    for (const member of mergedMentionMembers.value.members) {
      const fetched = fetchedAvatarUrlByJid.value[member.jid];
      const avatar = fetched ?? member.avatar_url;
      if (!(member.username in avatars) || avatar) {
        avatars[member.username] = avatar;
      }
    }

    for (const candidate of avatarCandidates.value) {
      const fetched = fetchedAvatarUrlByJid.value[candidate.jid];
      const avatar = fetched ?? candidate.avatar_url;
      if (!(candidate.nick in avatars) || avatar) {
        avatars[candidate.nick] = avatar;
      }
    }

    return avatars;
  });
  const membersWithAvatars = computed<MemberSummary[]>(() =>
    displayedMembers.value.map((member) => ({
      ...member,
      avatar_url: fetchedAvatarUrlByJid.value[member.jid] ?? member.avatar_url,
    })),
  );
  // XEP-0317 hats are server-emitted descriptive metadata only.
  // No client-side fabrication: owner / admin / moderator state
  // flows separately as `authorAuthorityByNick` below (XEP-0045
  // affiliation + role), and the UI renders the two layers
  // independently in MessageCard.vue.
  const authorHatsByNick = computed(() => messaging.roomHats.value);

  // Per-occupant MUC authority (affiliation + role) sourced live
  // from each inbound MUC presence. Distinct from `authorHatsByNick`:
  // authority is XEP-0045 and server-enforced; hats are XEP-0317
  // descriptive metadata with no protocol semantics. UI surfaces
  // that render OWNER / ADMIN / MOD chips read from here.
  const authorAuthorityByNick = computed(() => messaging.roomAuthority.value);

  watch(
    () => [xmppClient.value, avatarCandidates.value.map((candidate) => candidate.jid).join("\n")] as const,
    ([client]) => {
      if (!client) return;
      for (const candidate of avatarCandidates.value) {
        const jid = candidate.jid;
        if (!jid || candidate.avatar_url || avatarFetchStateByJid.value[jid]) continue;
        avatarFetchStateByJid.value = { ...avatarFetchStateByJid.value, [jid]: "pending" };
        void client.fetchUserAvatar(jid)
          .then((avatarUrl) => {
            fetchedAvatarUrlByJid.value = { ...fetchedAvatarUrlByJid.value, [jid]: avatarUrl };
          })
          .catch(() => {
            fetchedAvatarUrlByJid.value = { ...fetchedAvatarUrlByJid.value, [jid]: null };
          })
          .finally(() => {
            avatarFetchStateByJid.value = { ...avatarFetchStateByJid.value, [jid]: "done" };
          });
      }
    },
    { immediate: true },
  );

  return {
    authorJidByNick,
    mentionCandidates,
    inferredMemberJids,
    displayedMemberCount,
    displayedMemberState,
    memberCountLabel,
    avatarUrlByAuthor,
    membersWithAvatars,
    authorHatsByNick,
    authorAuthorityByNick,
  };
}
