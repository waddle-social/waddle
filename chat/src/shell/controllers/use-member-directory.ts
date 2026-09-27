import { computed, type Ref, watch } from "vue";
import type { useChannelMessages } from "@/channels/messages";
import type { useWaddleDirectory, MemberLoadState } from "@/waddles/directory";
import type { MemberSummary } from "@/lib/chat-types";
import { mentionAutocompleteCandidates, mergeMentionMembers } from "@/lib/mentions";

interface MemberDirectoryDeps {
  waddles: ReturnType<typeof useWaddleDirectory>;
  messaging: ReturnType<typeof useChannelMessages>;
  memberJidByNick: Ref<Record<string, string>>;
  mentionJidsByNickForSend: Ref<Record<string, string>>;
}

/**
 * Member roster and mention-autocomplete for the active channel: merges
 * authoritative affiliation lists with live MUC presence and keeps the
 * outbound mention JID map in sync. Avatars are not tracked here — every
 * surface reads the JID-keyed avatar store (`@/lib/avatars`).
 */
export function useMemberDirectory(deps: MemberDirectoryDeps) {
  const { waddles, messaging, memberJidByNick, mentionJidsByNickForSend } = deps;

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
  const mentionCandidates = computed(() => mentionAutocompleteCandidates(mergedMentionMembers.value.members));
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

  return {
    authorJidByNick,
    mentionCandidates,
    inferredMemberJids,
    displayedMemberCount,
    displayedMemberState,
    memberCountLabel,
    displayedMembers,
    authorHatsByNick,
    authorAuthorityByNick,
  };
}
