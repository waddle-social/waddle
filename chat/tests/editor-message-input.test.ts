import { describe, expect, test } from "bun:test";
import { Editor } from "@tiptap/vue-3";
import { shouldSendOnEnter } from "../src/lib/editor/chat-enter-key";
import { createChatEditorExtensions } from "../src/lib/editor/chat-editor-extensions";
import { richMessageToTiptap, tiptapToRichMessage, type MarkupSpan } from "../src/lib/rich-message";

if (typeof globalThis.requestAnimationFrame !== "function") {
  globalThis.requestAnimationFrame = ((callback: FrameRequestCallback) => {
    callback(performance.now());
    return 0;
  }) as typeof globalThis.requestAnimationFrame;
}

const enter = {
  key: "Enter",
  shiftKey: false,
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  isComposing: false,
};

function createEditor(content: Record<string, unknown>) {
  return new Editor({
    extensions: createChatEditorExtensions({ includePlaceholder: false }),
    content,
  });
}

describe("message input editor", () => {
  test("uses Enter to send normal paragraphs", () => {
    const editor = createEditor({
      type: "doc",
      content: [{ type: "paragraph", content: [{ type: "text", text: "hello" }] }],
    });

    try {
      expect(shouldSendOnEnter(editor, enter)).toBe(true);
    } finally {
      editor.destroy();
    }
  });

  test("leaves Enter available to list items and code blocks", () => {
    const listEditor = createEditor({
      type: "doc",
      content: [
        {
          type: "bulletList",
          content: [
            {
              type: "listItem",
              content: [{ type: "paragraph", content: [{ type: "text", text: "first" }] }],
            },
          ],
        },
      ],
    });
    const codeEditor = createEditor({
      type: "doc",
      content: [{ type: "codeBlock", content: [{ type: "text", text: "const x = 1" }] }],
    });

    try {
      expect(shouldSendOnEnter(listEditor, enter)).toBe(false);
      expect(shouldSendOnEnter(codeEditor, enter)).toBe(false);
    } finally {
      listEditor.destroy();
      codeEditor.destroy();
    }
  });

  test("uses Tiptap default list item schema and keymap", () => {
    const editor = createEditor({ type: "doc", content: [{ type: "paragraph" }] });

    try {
      expect(editor.schema.nodes.listItem.spec.content).toBe("paragraph block*");
      expect(editor.extensionManager.extensions.some((extension) => extension.name === "listKeymap")).toBe(true);
    } finally {
      editor.destroy();
    }
  });

  test("serializes lists without blank lines between sibling items", () => {
    const doc = {
      type: "doc",
      content: [
        {
          type: "bulletList",
          content: [
            { type: "listItem", content: [{ type: "paragraph", content: [{ type: "text", text: "one" }] }] },
            { type: "listItem", content: [{ type: "paragraph", content: [{ type: "text", text: "two" }] }] },
          ],
        },
      ],
    };

    const serialized = tiptapToRichMessage(doc);

    expect(serialized.body).toBe("- one\n- two");
    expect(serialized.markup).toEqual([
      { type: "list", start: 0, end: 11, ordered: false, items: [0, 6] },
    ]);
  });

  test("round-trips nested lists through rich markup", () => {
    const doc = {
      type: "doc",
      content: [
        {
          type: "bulletList",
          content: [
            {
              type: "listItem",
              content: [
                { type: "paragraph", content: [{ type: "text", text: "parent" }] },
                {
                  type: "bulletList",
                  content: [
                    { type: "listItem", content: [{ type: "paragraph", content: [{ type: "text", text: "child" }] }] },
                  ],
                },
              ],
            },
          ],
        },
      ],
    };

    const serialized = tiptapToRichMessage(doc);
    const hydrated = richMessageToTiptap(serialized);

    expect(serialized.body).toBe("- parent\n  - child");
    expect(tiptapToRichMessage(hydrated).body).toBe(serialized.body);
  });

  test("removing a list item keeps the following item as a sibling after save", () => {
    const body = "- one\n- three";
    const markup: MarkupSpan[] = [
      { type: "list", start: 0, end: body.length, ordered: false, items: [0, 6] },
    ];

    const editor = createEditor(richMessageToTiptap({ body, markup }));

    try {
      expect(tiptapToRichMessage(editor.getJSON() as any).body).toBe(body);
    } finally {
      editor.destroy();
    }
  });

  test("serializes inline marks and links as XMPP metadata", () => {
    const doc = {
      type: "doc",
      content: [
        {
          type: "paragraph",
          content: [
            { type: "text", text: "Hi " },
            { type: "text", text: "there", marks: [{ type: "bold" }, { type: "italic" }] },
            { type: "text", text: " docs", marks: [{ type: "link", attrs: { href: "https://example.com/docs" } }] },
          ],
        },
      ],
    };

    const serialized = tiptapToRichMessage(doc);

    expect(serialized.body).toBe("Hi there docs");
    expect(serialized.markup).toEqual([
      { type: "span", start: 3, end: 8, styles: ["emphasis", "strong"] },
    ]);
    expect(serialized.references).toEqual([
      { type: "data", uri: "https://example.com/docs", begin: 8, end: 13 },
    ]);
  });

  test("uses code point offsets for emoji", () => {
    const doc = {
      type: "doc",
      content: [
        {
          type: "paragraph",
          content: [
            { type: "text", text: "👋 " },
            { type: "text", text: "bold", marks: [{ type: "bold" }] },
          ],
        },
      ],
    };

    expect(tiptapToRichMessage(doc).markup).toEqual([
      { type: "span", start: 2, end: 6, styles: ["strong"] },
    ]);
  });
});
