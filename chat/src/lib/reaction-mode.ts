export const QUICK_REACTION_EMOJIS: readonly string[] = ["👍", "❤️", "😂", "🎉", "👀"];

export type ReactionModeScope = "feed" | "thread";
export type ReactionModeMessage = {
  id: string;
  createdAt: string;
  threadId?: string;
  isRetracted?: boolean;
  deliveryStatus?: "queued" | "sending" | "delivered" | "failed";
  canReact?: boolean;
};

function isReactionModeMessageEligible(message: ReactionModeMessage, scope: ReactionModeScope): boolean {
  if (message.isRetracted) return false;
  if (message.canReact === false) return false;
  if (message.deliveryStatus === "queued" || message.deliveryStatus === "sending" || message.deliveryStatus === "failed") {
    return false;
  }
  return scope === "thread" || !message.threadId || message.threadId === message.id;
}

export function reactionModeMessages(
  messages: readonly ReactionModeMessage[],
  scope: ReactionModeScope,
): ReactionModeMessage[] {
  return messages.filter((message) => isReactionModeMessageEligible(message, scope));
}

export function selectInitialReactionMessage(
  messages: readonly ReactionModeMessage[],
  scope: ReactionModeScope,
): string | null {
  const eligible = reactionModeMessages(messages, scope);
  if (eligible.length === 0) return null;

  return eligible.reduce((selected, candidate) => {
    return createdAtTime(candidate) >= createdAtTime(selected) ? candidate : selected;
  }).id;
}

export function preserveReactionSelection(
  previousId: string | null,
  messages: readonly ReactionModeMessage[],
  scope: ReactionModeScope,
): string | null {
  const eligible = reactionModeMessages(messages, scope);
  if (eligible.length === 0) return null;
  if (previousId && eligible.some((message) => message.id === previousId)) return previousId;
  return selectInitialReactionMessage(eligible, scope);
}

export function moveReactionSelection(
  currentId: string | null,
  messages: readonly ReactionModeMessage[],
  scope: ReactionModeScope,
  direction: "previous" | "next",
): string | null {
  const eligible = reactionModeMessages(messages, scope);
  if (eligible.length === 0) return null;
  if (!currentId) return selectInitialReactionMessage(eligible, scope);

  const selectedIndex = eligible.findIndex((message) => message.id === currentId);
  if (selectedIndex === -1) return selectInitialReactionMessage(eligible, scope);

  const nextIndex = direction === "previous"
    ? Math.max(0, selectedIndex - 1)
    : Math.min(eligible.length - 1, selectedIndex + 1);

  return eligible[nextIndex]?.id ?? null;
}

export function quickReactionForKey(key: string): string | null {
  if (!/^[1-5]$/.test(key)) return null;
  return QUICK_REACTION_EMOJIS[Number(key) - 1] ?? null;
}

function createdAtTime(message: ReactionModeMessage): number {
  const time = Date.parse(message.createdAt);
  return Number.isNaN(time) ? 0 : time;
}
