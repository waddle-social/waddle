export interface ChannelAdmissionPolicy {
  isPublic: boolean;
  membersOnly: boolean;
}

export function requireMembershipForUnlistedChannel(
  policy: ChannelAdmissionPolicy,
): ChannelAdmissionPolicy {
  if (policy.isPublic || policy.membersOnly) return policy;
  return { ...policy, membersOnly: true };
}
