import { describe, expect, test } from "bun:test";
import { getComposerEscapeAction, getReplyJumpNotice, REPLY_JUMP_TARGET_MISSING_MESSAGE } from "../src/lib/reply-ux";

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

  test("missing reply jump targets show a calm notice", () => {
    expect(getReplyJumpNotice(true)).toBe("");
    expect(getReplyJumpNotice(false)).toBe(REPLY_JUMP_TARGET_MISSING_MESSAGE);
  });
});
