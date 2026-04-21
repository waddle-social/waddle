import { describe, expect, test } from "bun:test";
import { Editor } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import { ChatLink } from "../src/lib/editor/chat-link";

if (typeof globalThis.requestAnimationFrame !== "function") {
  globalThis.requestAnimationFrame = ((callback: FrameRequestCallback) => {
    callback(performance.now());
    return 0;
  }) as typeof globalThis.requestAnimationFrame;
}

describe("ChatLink", () => {
  test("does not carry pasted link marks onto later text", () => {
    const url = "https://example.com";
    const editor = new Editor({
      extensions: [
        StarterKit.configure({ link: false }),
        ChatLink.configure({ autolink: true }),
      ],
      content: {
        type: "doc",
        content: [
          {
            type: "paragraph",
            content: [
              {
                type: "text",
                text: url,
                marks: [{ type: "link", attrs: { href: url } }],
              },
            ],
          },
        ],
      },
    });

    try {
      editor.commands.setTextSelection(1 + url.length);
      editor.commands.insertContent({ type: "text", text: " after" });

      const paragraph = editor.getJSON().content?.[0];
      const content = paragraph?.content ?? [];

      expect(content).toHaveLength(2);
      expect(content[0]).toMatchObject({
        type: "text",
        text: url,
        marks: [{ type: "link", attrs: expect.objectContaining({ href: url }) }],
      });
      expect(content[1]).toEqual({ type: "text", text: " after" });
    } finally {
      editor.destroy();
    }
  });
});
