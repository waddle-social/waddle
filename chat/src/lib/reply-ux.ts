export const REPLY_JUMP_TARGET_MISSING_MESSAGE = "Original message isn't in this loaded history yet.";

export function getReplyJumpNotice(targetFound: boolean): string {
  return targetFound ? "" : REPLY_JUMP_TARGET_MISSING_MESSAGE;
}

export function getComposerEscapeAction(state: {
  showMentions: boolean;
  showEmoji: boolean;
  showCommands?: boolean;
  isReplyingTo: boolean;
}): "dismiss-autocomplete" | "cancel-reply" | "none" {
  if (state.showMentions || state.showEmoji || state.showCommands) {
    return "dismiss-autocomplete";
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
  showCommands?: boolean;
  commandCount?: number;
}): "select-mention" | "select-emoji" | "select-command" | "dismiss-autocomplete" | "none" {
  if (state.showMentions) {
    return state.mentionCount > 0 ? "select-mention" : "dismiss-autocomplete";
  }

  if (state.showEmoji) {
    return state.emojiCount > 0 ? "select-emoji" : "dismiss-autocomplete";
  }

  if (state.showCommands) {
    return (state.commandCount ?? 0) > 0 ? "select-command" : "dismiss-autocomplete";
  }

  return "none";
}
