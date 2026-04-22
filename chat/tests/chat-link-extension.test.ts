import { describe, expect, test } from "bun:test";
import { Editor } from "@tiptap/core";
import { Fragment, Slice } from "@tiptap/pm/model";
import StarterKit from "@tiptap/starter-kit";
import { createChatEditorExtensions } from "../src/lib/editor/chat-editor-extensions";
import { ChatLink } from "../src/lib/editor/chat-link";

if (typeof globalThis.requestAnimationFrame !== "function") {
  globalThis.requestAnimationFrame = ((callback: FrameRequestCallback) => {
    callback(performance.now());
    return 0;
  }) as typeof globalThis.requestAnimationFrame;
}

describe("ChatLink", () => {
  test("links a URL pasted into an empty selection", () => {
    const url = "https://example.com/docs";
    const editor = new Editor({
      enableCoreExtensions: { keymap: false },
      extensions: createChatEditorExtensions({ includePlaceholder: false }),
      content: {
        type: "doc",
        content: [{ type: "paragraph" }],
      },
    });

    try {
      editor.commands.setTextSelection(1);

      const pastePlugin = editor.extensionManager.plugins.find((plugin) => plugin.key.startsWith("chatLinkPasteUrl"));
      const handled = pastePlugin?.props.handlePaste?.(
        editor.view,
        {} as ClipboardEvent,
        new Slice(Fragment.from(editor.schema.text(url)), 0, 0),
      );

      expect(handled).toBe(true);

      const paragraph = editor.getJSON().content?.[0];
      expect(paragraph?.content?.[0]).toMatchObject({
        type: "text",
        text: url,
        marks: [{ type: "link", attrs: expect.objectContaining({ href: url }) }],
      });
    } finally {
      editor.destroy();
    }
  });

  test("adds the default protocol when pasting a bare URL", () => {
    const editor = new Editor({
      enableCoreExtensions: { keymap: false },
      extensions: createChatEditorExtensions({ includePlaceholder: false }),
      content: {
        type: "doc",
        content: [{ type: "paragraph" }],
      },
    });

    try {
      editor.commands.setTextSelection(1);

      const pastePlugin = editor.extensionManager.plugins.find((plugin) => plugin.key.startsWith("chatLinkPasteUrl"));
      const handled = pastePlugin?.props.handlePaste?.(
        editor.view,
        {} as ClipboardEvent,
        new Slice(Fragment.from(editor.schema.text("example.com/docs")), 0, 0),
      );

      expect(handled).toBe(true);

      const paragraph = editor.getJSON().content?.[0];
      expect(paragraph?.content?.[0]).toMatchObject({
        type: "text",
        text: "example.com/docs",
        marks: [{ type: "link", attrs: expect.objectContaining({ href: "http://example.com/docs" }) }],
      });
    } finally {
      editor.destroy();
    }
  });

  test("links selected text when a URL is pasted", () => {
    const url = "https://example.com/docs";
    const editor = new Editor({
      enableCoreExtensions: { keymap: false },
      extensions: createChatEditorExtensions({ includePlaceholder: false }),
      content: {
        type: "doc",
        content: [{ type: "paragraph", content: [{ type: "text", text: "example" }] }],
      },
    });

    try {
      editor.commands.setTextSelection({ from: 1, to: 8 });

      const pastePlugin = editor.extensionManager.plugins.find((plugin) => plugin.key.startsWith("handlePasteLink"));
      const handled = pastePlugin?.props.handlePaste?.(
        editor.view,
        {} as ClipboardEvent,
        new Slice(Fragment.from(editor.schema.text(url)), 0, 0),
      );

      expect(handled).toBe(true);

      const paragraph = editor.getJSON().content?.[0];
      expect(paragraph?.content?.[0]).toMatchObject({
        type: "text",
        text: "example",
        marks: [{ type: "link", attrs: expect.objectContaining({ href: url }) }],
      });
    } finally {
      editor.destroy();
    }
  });

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
