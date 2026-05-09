export const REPLY_JUMP_TARGET_MISSING_MESSAGE = "Original message isn't in this loaded history yet.";

export function getReplyJumpNotice(targetFound: boolean): string {
  return targetFound ? "" : REPLY_JUMP_TARGET_MISSING_MESSAGE;
}

export function getComposerEscapeAction(state: {
  showMentions: boolean;
  showEmoji: boolean;
  showSlash?: boolean;
  isReplyingTo: boolean;
}): "dismiss-autocomplete" | "dismiss-slash" | "cancel-reply" | "none" {
  if (state.showMentions || state.showEmoji) {
    return "dismiss-autocomplete";
  }

  if (state.showSlash) {
    return "dismiss-slash";
  }

  if (state.isReplyingTo) {
    return "cancel-reply";
  }

  return "none";
}

export function getComposerAutocompleteAction(state: {
  showMentions: boolean;
  mentionCount: number;
  showEmoji: boolean;
  emojiCount: number;
  showSlash?: boolean;
  slashHasPrefix?: boolean;
  slashCandidateCount?: number;
  slashHasResolution?: boolean;
}): "select-mention" | "select-emoji" | "select-command" | "submit-slash" | "block-slash" | "dismiss-autocomplete" | "none" {
  if (state.showMentions) {
    return state.mentionCount > 0 ? "select-mention" : "dismiss-autocomplete";
  }

  if (state.showEmoji) {
    return state.emojiCount > 0 ? "select-emoji" : "dismiss-autocomplete";
  }

  if (state.showSlash) {
    if (state.slashHasResolution) return "submit-slash";
    if ((state.slashCandidateCount ?? 0) > 0) return "select-command";
    // Block only when the user typed an actual command attempt. A bare `/`
    // with nothing matching falls through so the Enter sends as plain text.
    if (state.slashHasPrefix) return "block-slash";
    return "none";
  }

  return "none";
}
