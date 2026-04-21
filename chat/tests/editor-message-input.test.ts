import { describe, expect, test } from "bun:test";
import { Editor } from "@tiptap/vue-3";
import StarterKit from "@tiptap/starter-kit";
import { shouldSendOnEnter } from "../src/lib/editor/chat-enter-key";
import { parseXep0393ToTiptap } from "../src/lib/editor/xep0393-parser";
import { serializeTiptapToXep0393 } from "../src/lib/editor/xep0393-serializer";

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
    extensions: [
      StarterKit.configure({
        heading: false,
        horizontalRule: false,
        link: false,
        underline: false,
      }),
    ],
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

  test("round-trips bullet and ordered lists through edit content", () => {
    const bulletDoc = parseXep0393ToTiptap("- one\n- *two*");
    const orderedDoc = parseXep0393ToTiptap("3. one\n4. two");

    expect(serializeTiptapToXep0393(bulletDoc as any).body).toBe("- one\n- *two*");
    expect(serializeTiptapToXep0393(orderedDoc as any).body).toBe("3. one\n4. two");
  });
});
