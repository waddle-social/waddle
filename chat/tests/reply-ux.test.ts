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
      }),
    ).toBe("select-emoji");

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

  test("Escape dismisses the slash popover before cancelling a reply", () => {
    expect(
      getComposerEscapeAction({
        showMentions: false,
        showEmoji: false,
        showSlash: true,
        isReplyingTo: true,
      }),
    ).toBe("dismiss-slash");
  });

  test("Escape ignores slash popover when mention/emoji autocomplete is also showing", () => {
    expect(
      getComposerEscapeAction({
        showMentions: true,
        showEmoji: false,
        showSlash: true,
        isReplyingTo: false,
      }),
    ).toBe("dismiss-autocomplete");
  });

  test("Submit on `/word` that resolves to a command invokes the slash submit path", () => {
    expect(
      getComposerAutocompleteAction({
        showMentions: false,
        mentionCount: 0,
        showEmoji: false,
        emojiCount: 0,
        showSlash: true,
        slashCandidateCount: 1,
        slashHasResolution: true,
      }),
    ).toBe("submit-slash");
  });

  test("Submit on slash popover with candidates but no exact resolution expands the highlighted candidate", () => {
    expect(
      getComposerAutocompleteAction({
        showMentions: false,
        mentionCount: 0,
        showEmoji: false,
        emojiCount: 0,
        showSlash: true,
        slashCandidateCount: 2,
        slashHasResolution: false,
      }),
    ).toBe("select-command");
  });

  test("Submit on `/word` with no candidates and no resolution blocks", () => {
    expect(
      getComposerAutocompleteAction({
        showMentions: false,
        mentionCount: 0,
        showEmoji: false,
        emojiCount: 0,
        showSlash: true,
        slashHasPrefix: true,
        slashCandidateCount: 0,
        slashHasResolution: false,
      }),
    ).toBe("block-slash");
  });

  test("Bare `/` with zero candidates falls through to plain-text send", () => {
    expect(
      getComposerAutocompleteAction({
        showMentions: false,
        mentionCount: 0,
        showEmoji: false,
        emojiCount: 0,
        showSlash: true,
        slashHasPrefix: false,
        slashCandidateCount: 0,
        slashHasResolution: false,
      }),
    ).toBe("none");
  });

  test("Mention autocomplete still wins over slash popover", () => {
    expect(
      getComposerAutocompleteAction({
        showMentions: true,
        mentionCount: 1,
        showEmoji: false,
        emojiCount: 0,
        showSlash: true,
        slashCandidateCount: 1,
        slashHasResolution: true,
      }),
    ).toBe("select-mention");
  });
});
