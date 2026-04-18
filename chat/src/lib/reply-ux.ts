export const REPLY_JUMP_TARGET_MISSING_MESSAGE = "Original message isn't in this loaded history yet.";

export function getReplyJumpNotice(targetFound: boolean): string {
  return targetFound ? "" : REPLY_JUMP_TARGET_MISSING_MESSAGE;
}

export function getComposerEscapeAction(state: {
  showMentions: boolean;
  showEmoji: boolean;
  isReplyingTo: boolean;
}): "dismiss-autocomplete" | "cancel-reply" | "none" {
  if (state.showMentions || state.showEmoji) {
    return "dismiss-autocomplete";
  }

  if (state.isReplyingTo) {
    return "cancel-reply";
  }

  return "none";
}
