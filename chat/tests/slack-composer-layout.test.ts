import { describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import { composerPlaceholder } from "../src/components/chat/composer-placeholder";
import { sanitizeLinkUrl } from "../src/components/chat/composables/use-editor-link-input";
import {
  BLOCK_FORMAT_ACTIONS,
  INLINE_FORMAT_ACTIONS,
} from "../src/components/chat/editor-format-actions";

function composerProps(overrides: Record<string, unknown> = {}) {
  return {
    draft: "",
    channelName: "general",
    isForumChannel: false,
    isSending: false,
    disabled: false,
    mentionCandidates: [],
    slowModeCooldown: 0,
    uploadProgress: { uploading: false, progress: 0, filename: "" },
    ...overrides,
  };
}

describe("Slack-style composer layout", () => {
  test("renders one card with the add, format, emoji, mention, command and send actions", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/MessageComposer.vue",
      composerProps(),
      import.meta.url,
    );

    expect(html).toContain("chat-composer-card");
    expect(html).toContain("chat-composer-toolbar");
    const order = [
      'aria-label="Attach"',
      'aria-label="Show formatting"',
      'aria-label="Emoji"',
      'aria-label="Mention someone"',
      'aria-label="Run a command"',
      'aria-label="Send message"',
    ].map((label) => html.indexOf(label));
    for (const index of order) expect(index).toBeGreaterThan(-1);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
  });

  test("keeps extensions inside the + menu instead of a standalone button", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/MessageComposer.vue",
      composerProps(),
      import.meta.url,
    );

    expect(html).not.toContain('aria-label="Extensions"');
    expect(html).toContain('aria-haspopup="menu"');
  });

  test("send stays unarmed and disabled while the draft is empty", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/MessageComposer.vue",
      composerProps(),
      import.meta.url,
    );

    const send = html.slice(html.indexOf('class="chat-composer-send'));
    expect(send.slice(0, send.indexOf(">"))).toContain("disabled");
    expect(html).not.toContain("chat-composer-send--armed");
  });

  test("send arms once the draft has text", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/MessageComposer.vue",
      composerProps({ draft: "hello" }),
      import.meta.url,
    );

    expect(html).toContain("chat-composer-send--armed");
  });
});

describe("ComposerAddMenu", () => {
  test("lists upload, GIF and extensions", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/ComposerAddMenu.vue",
      { anchorEl: null, showExtensions: true },
      import.meta.url,
    );

    expect(html).toContain('role="menu"');
    expect(html).toContain("Upload from your computer");
    expect(html).toContain("GIF");
    expect(html).toContain("Extensions");
  });

  test("hides extensions on surfaces without a launcher", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/ComposerAddMenu.vue",
      { anchorEl: null, showExtensions: false },
      import.meta.url,
    );

    expect(html).toContain("Upload from your computer");
    expect(html).not.toContain("Extensions");
  });
});

describe("composerPlaceholder", () => {
  const base = {
    slowModeCooldown: 0,
    needsForumTitle: false,
    isForumChannel: false,
    channelName: "general",
  };

  test("defaults to the channel name", () => {
    expect(composerPlaceholder(base)).toBe("Message #general");
  });

  test("uses the surface override for DMs and threads", () => {
    expect(composerPlaceholder({ ...base, placeholder: "Message bob" })).toBe("Message bob");
    expect(composerPlaceholder({ ...base, placeholder: "Reply…" })).toBe("Reply…");
  });

  test("slow mode and forum states outrank the override", () => {
    expect(composerPlaceholder({ ...base, placeholder: "Reply…", slowModeCooldown: 5 })).toBe(
      "Slow mode — wait 5s",
    );
    expect(composerPlaceholder({ ...base, placeholder: "Reply…", needsForumTitle: true })).toBe(
      "Write the opening post",
    );
    expect(composerPlaceholder({ ...base, placeholder: "Reply…", isForumChannel: true })).toBe(
      "Reply in this topic",
    );
  });
});

describe("sanitizeLinkUrl", () => {
  test("accepts http, https and mailto", () => {
    expect(sanitizeLinkUrl(" https://example.com ")).toBe("https://example.com/");
    expect(sanitizeLinkUrl("http://example.com/a")).toBe("http://example.com/a");
    expect(sanitizeLinkUrl("mailto:a@example.com")).toBe("mailto:a@example.com");
  });

  test("rejects empty, malformed and script URLs", () => {
    expect(sanitizeLinkUrl("")).toBeNull();
    expect(sanitizeLinkUrl("not a url")).toBeNull();
    expect(sanitizeLinkUrl("javascript:alert(1)")).toBeNull();
  });
});

describe("editor format actions", () => {
  test("the fixed bar and bubble toolbar share one ordered command set", () => {
    expect(INLINE_FORMAT_ACTIONS.map((a) => a.name)).toEqual(["bold", "italic", "strike"]);
    expect(BLOCK_FORMAT_ACTIONS.map((a) => a.name)).toEqual([
      "ordered-list",
      "bullet-list",
      "blockquote",
      "code",
      "code-block",
    ]);
  });

  test("each action toggles its TipTap command on a focused chain", () => {
    const calls: string[] = [];
    const chain: Record<string, () => unknown> = new Proxy({}, {
      get: (_target, key: string) => () => {
        calls.push(key);
        return chain;
      },
    });
    const editor = { chain: () => chain, isActive: (name: string) => name === "bold" } as never;

    for (const action of [...INLINE_FORMAT_ACTIONS, ...BLOCK_FORMAT_ACTIONS]) action.run(editor);

    expect(calls.filter((c) => c.startsWith("toggle"))).toEqual([
      "toggleBold",
      "toggleItalic",
      "toggleStrike",
      "toggleOrderedList",
      "toggleBulletList",
      "toggleBlockquote",
      "toggleCode",
      "toggleCodeBlock",
    ]);
    expect(calls.filter((c) => c === "focus")).toHaveLength(8);
    expect(INLINE_FORMAT_ACTIONS[0].isActive(editor)).toBe(true);
    expect(INLINE_FORMAT_ACTIONS[1].isActive(editor)).toBe(false);
  });
});
