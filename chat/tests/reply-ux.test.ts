import { describe, expect, test } from "bun:test";
import {
  getComposerAutocompleteAction,
  getComposerEscapeAction,
  getReplyJumpNotice,
  REPLY_JUMP_TARGET_MISSING_MESSAGE,
} from "../src/lib/reply-ux";

describe("reply UX helpers", () => {
  test("Escape dismisses autocomplete before cancelling reply context", () => {
    expect(
      getComposerEscapeAction({
        showMentions: true,
        showEmoji: false,
        isReplyingTo: true,
      }),
    ).toBe("dismiss-autocomplete");

    expect(
      getComposerEscapeAction({
        showMentions: false,
        showEmoji: true,
        isReplyingTo: true,
      }),
    ).toBe("dismiss-autocomplete");
  });

  test("Escape cancels the active reply when autocomplete is closed", () => {
    expect(
      getComposerEscapeAction({
        showMentions: false,
        showEmoji: false,
        isReplyingTo: true,
      }),
    ).toBe("cancel-reply");
  });

  test("autocomplete only consumes submit when a selectable result exists", () => {
    expect(
      getComposerAutocompleteAction({
        showMentions: true,
        mentionCount: 1,
        showEmoji: false,
        emojiCount: 0,
      }),
    ).toBe("select-mention");

    expect(
      getComposerAutocompleteAction({
        showMentions: false,
        mentionCount: 0,
        showEmoji: true,
        emojiCount: 1,
        showCommands: false,
        commandCount: 0,
      }),
    ).toBe("select-emoji");

    expect(
      getComposerAutocompleteAction({
        showMentions: false,
        mentionCount: 0,
        showEmoji: false,
        emojiCount: 0,
        showCommands: true,
        commandCount: 1,
      }),
    ).toBe("select-command");

    expect(
      getComposerAutocompleteAction({
        showMentions: true,
        mentionCount: 0,
        showEmoji: false,
        emojiCount: 0,
      }),
    ).toBe("dismiss-autocomplete");

    expect(
      getComposerAutocompleteAction({
        showMentions: false,
        mentionCount: 0,
        showEmoji: false,
        emojiCount: 0,
      }),
    ).toBe("none");
  });

  test("missing reply jump targets show a calm notice", () => {
    expect(getReplyJumpNotice(true)).toBe("");
    expect(getReplyJumpNotice(false)).toBe(REPLY_JUMP_TARGET_MISSING_MESSAGE);
  });
});
