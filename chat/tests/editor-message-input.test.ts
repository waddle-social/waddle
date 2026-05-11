import { describe, expect, test } from "bun:test";
import { Editor } from "@tiptap/core";
import { createChatEditorExtensions } from "../src/lib/editor/chat-editor-extensions";
import { chatKeymapPluginKey, isChatSubmitBoundary } from "../src/lib/editor/chat-keymap";
import { richMessageToMarkdown, richMessageToTiptap, tiptapToRichMessage, type MarkupSpan } from "../src/lib/rich-message";

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
  keyCode: 13,
};

function createEditor(content: Record<string, unknown>, onSubmit: (doc: Record<string, unknown>) => boolean | void = () => false) {
  const editor = new Editor({
    enableCoreExtensions: { keymap: false },
    extensions: createChatEditorExtensions({
      includePlaceholder: false,
      onSubmit,
    }),
    content,
  });
  editor.commands.setTextSelection(editor.state.doc.content.size);
  return editor;
}

function dispatchChatEnter(editor: Editor, overrides: Partial<typeof enter> = {}) {
  const plugin = editor.extensionManager.plugins.find((candidate) => candidate.key === chatKeymapPluginKey.key);
  const handler = plugin?.props.handleKeyDown;
  expect(handler).toBeDefined();
  return handler?.(editor.view, { ...enter, ...overrides } as KeyboardEvent);
}

function dispatchEditorKeydown(editor: Editor, event: Partial<typeof enter> = {}) {
  const keyEvent = { ...enter, ...event } as KeyboardEvent;
  for (const plugin of editor.extensionManager.plugins) {
    const before = JSON.stringify(editor.getJSON());
    const handled = plugin.props.handleKeyDown?.(editor.view, keyEvent);
    if (handled || before !== JSON.stringify(editor.getJSON())) return true;
  }
  return false;
}

function dispatchTextInput(editor: Editor, text: string) {
  const { from, to } = editor.state.selection;
  for (const plugin of editor.extensionManager.plugins) {
    const handled = plugin.props.handleTextInput?.(editor.view, from, to, text);
    if (handled) return true;
  }

  editor.view.dispatch(editor.state.tr.insertText(text, from, to));
  return false;
}

describe("message input editor", () => {
  test("uses Enter to send normal paragraphs", () => {
    let submitted: Record<string, unknown> | undefined;
    const editor = createEditor(
      {
        type: "doc",
        content: [{ type: "paragraph", content: [{ type: "text", text: "hello" }] }],
      },
      (doc) => {
        submitted = doc;
      },
    );

    try {
      expect(isChatSubmitBoundary(editor)).toBe(true);
      expect(dispatchEditorKeydown(editor)).toBe(true);
      expect(submitted).toEqual(editor.getJSON());
    } finally {
      editor.destroy();
    }
  });

  test("installs the chat keymap after structural Tiptap shortcuts", () => {
    const editor = createEditor({
      type: "doc",
      content: [{ type: "paragraph" }],
    });

    try {
      const extensionNames = editor.extensionManager.extensions.map((extension) => extension.name);
      const chatKeymap = editor.extensionManager.extensions.find((extension) => extension.name === "keymap");

      expect(extensionNames.filter((name) => name === "keymap")).toHaveLength(1);
      expect(chatKeymap?.config.priority).toBe(99);
      expect(extensionNames.indexOf("listItem")).toBeLessThan(extensionNames.indexOf("keymap"));
      expect(extensionNames.indexOf("codeBlock")).toBeLessThan(extensionNames.indexOf("keymap"));
    } finally {
      editor.destroy();
    }
  });

  test("does not submit from rich block contexts", () => {
    const listEditor = createEditor({
      type: "doc",
      content: [
        {
          type: "bulletList",
          content: [
            { type: "listItem", content: [{ type: "paragraph", content: [{ type: "text", text: "first" }] }] },
          ],
        },
      ],
    });
    const orderedListEditor = createEditor({
      type: "doc",
      content: [
        {
          type: "orderedList",
          content: [
            { type: "listItem", content: [{ type: "paragraph", content: [{ type: "text", text: "first" }] }] },
          ],
        },
      ],
    });
    const quoteEditor = createEditor({
      type: "doc",
      content: [{ type: "blockquote", content: [{ type: "paragraph", content: [{ type: "text", text: "quoted" }] }] }],
    });
    const codeEditor = createEditor({
      type: "doc",
      content: [{ type: "codeBlock", attrs: { language: "ts" }, content: [{ type: "text", text: "const x = 1" }] }],
    });

    try {
      expect(isChatSubmitBoundary(listEditor)).toBe(false);
      expect(isChatSubmitBoundary(orderedListEditor)).toBe(false);
      expect(isChatSubmitBoundary(quoteEditor)).toBe(false);
      expect(isChatSubmitBoundary(codeEditor)).toBe(false);
    } finally {
      listEditor.destroy();
      orderedListEditor.destroy();
      quoteEditor.destroy();
      codeEditor.destroy();
    }
  });

  test("leaves Enter to Tiptap while editing in the middle of a document", () => {
    const editor = createEditor({
      type: "doc",
      content: [{ type: "paragraph", content: [{ type: "text", text: "hello world" }] }],
    });

    try {
      editor.commands.setTextSelection(6);
      expect(isChatSubmitBoundary(editor)).toBe(false);
      expect(dispatchEditorKeydown(editor)).toBe(true);
      expect(editor.getJSON().content).toHaveLength(2);
    } finally {
      editor.destroy();
    }
  });

  test("uses Enter to send when a formatted trailing selection remains active", () => {
    let submitted: Record<string, unknown> | undefined;
    const editor = createEditor(
      {
        type: "doc",
        content: [{ type: "paragraph", content: [{ type: "text", text: "hello world" }] }],
      },
      (doc) => {
        submitted = doc;
      },
    );

    try {
      editor.commands.setTextSelection({ from: 7, to: 12 });
      editor.commands.toggleBold();
      expect(editor.state.selection.empty).toBe(false);
      expect(isChatSubmitBoundary(editor)).toBe(true);
      expect(dispatchEditorKeydown(editor)).toBe(true);
      expect(submitted).toEqual(editor.getJSON());
    } finally {
      editor.destroy();
    }
  });

  test("leaves Enter to Tiptap when selected text has content after it", () => {
    const editor = createEditor({
      type: "doc",
      content: [{ type: "paragraph", content: [{ type: "text", text: "hello world" }] }],
    });

    try {
      editor.commands.setTextSelection({ from: 1, to: 6 });
      expect(isChatSubmitBoundary(editor)).toBe(false);
      expect(dispatchEditorKeydown(editor)).toBe(true);
      expect(editor.getJSON().content).toHaveLength(2);
    } finally {
      editor.destroy();
    }
  });

  test("uses Enter to send from the trailing boundary after exiting a list", () => {
    let submitted: Record<string, unknown> | undefined;
    const editor = createEditor(
      {
        type: "doc",
        content: [
          { type: "paragraph", content: [{ type: "text", text: "hello" }] },
          {
            type: "bulletList",
            content: [
              { type: "listItem", content: [{ type: "paragraph", content: [{ type: "text", text: "one" }] }] },
            ],
          },
          { type: "paragraph" },
        ],
      },
      (doc) => {
        submitted = doc;
      },
    );

    try {
      let trailingParagraphStart = 0;
      editor.state.doc.descendants((node, pos) => {
        if (node.type.name === "paragraph" && pos > trailingParagraphStart) trailingParagraphStart = pos;
      });

      editor.commands.setTextSelection(trailingParagraphStart + 1);

      expect(editor.isActive("bulletList")).toBe(false);
      expect(editor.isActive("listItem")).toBe(false);
      expect(isChatSubmitBoundary(editor)).toBe(true);
      expect(dispatchEditorKeydown(editor)).toBe(true);
      expect(submitted).toEqual(editor.getJSON());
    } finally {
      editor.destroy();
    }
  });

  test("uses Enter to send before trailing empty paragraphs", () => {
    let submitted: Record<string, unknown> | undefined;
    const editor = createEditor(
      {
        type: "doc",
        content: [
          {
            type: "bulletList",
            content: [
              { type: "listItem", content: [{ type: "paragraph", content: [{ type: "text", text: "one" }] }] },
            ],
          },
          { type: "paragraph" },
          { type: "paragraph" },
        ],
      },
      (doc) => {
        submitted = doc;
      },
    );

    try {
      let firstTrailingParagraph = 0;
      editor.state.doc.descendants((node, pos) => {
        if (node.type.name === "paragraph" && !node.textContent && firstTrailingParagraph === 0) {
          firstTrailingParagraph = pos;
          return false;
        }
        return true;
      });

      editor.commands.setTextSelection(firstTrailingParagraph + 1);

      expect(isChatSubmitBoundary(editor)).toBe(true);
      expect(dispatchEditorKeydown(editor)).toBe(true);
      expect(submitted).toEqual(editor.getJSON());
    } finally {
      editor.destroy();
    }
  });

  test("keeps Shift+Enter and composing Enter out of chat submit", () => {
    let submitted: Record<string, unknown> | undefined;
    const editor = createEditor(
      {
        type: "doc",
        content: [{ type: "paragraph", content: [{ type: "text", text: "hello" }] }],
      },
      (doc) => {
        submitted = doc;
      },
    );

    try {
      expect(dispatchEditorKeydown(editor, { shiftKey: true })).toBe(true);
      expect(submitted).toBeUndefined();
      expect(tiptapToRichMessage(editor.getJSON()).body).toBe("hello\n");

      expect(dispatchChatEnter(editor, { isComposing: true, keyCode: 229 })).toBe(false);
      expect(submitted).toBeUndefined();
    } finally {
      editor.destroy();
    }
  });

  test("lets Tiptap list input rules and list exit run before chat submit", () => {
    let submitted: Record<string, unknown> | undefined;
    const editor = createEditor(
      { type: "doc", content: [{ type: "paragraph" }] },
      (doc) => {
        submitted = doc;
      },
    );

    try {
      expect(dispatchTextInput(editor, "*")).toBe(false);
      expect(dispatchTextInput(editor, " ")).toBe(true);
      expect(editor.isActive("bulletList")).toBe(true);
      expect(editor.isActive("listItem")).toBe(true);

      expect(dispatchTextInput(editor, "one")).toBe(false);
      expect(dispatchEditorKeydown(editor)).toBe(true);
      expect(submitted).toBeUndefined();
      expect(editor.isActive("bulletList")).toBe(true);
      expect(editor.isActive("listItem")).toBe(true);

      expect(dispatchEditorKeydown(editor)).toBe(true);
      expect(submitted).toBeUndefined();
      expect(editor.isActive("bulletList")).toBe(false);
      expect(editor.isActive("listItem")).toBe(false);

      expect(dispatchEditorKeydown(editor)).toBe(true);
      expect(submitted).toEqual(editor.getJSON());
    } finally {
      editor.destroy();
    }
  });

  test("sends on the Enter after leaving supported block contexts", () => {
    const cases = [
      {
        name: "bulletList",
        content: {
          type: "doc",
          content: [
            {
              type: "bulletList",
              content: [
                { type: "listItem", content: [{ type: "paragraph", content: [{ type: "text", text: "one" }] }] },
              ],
            },
          ],
        },
        activeType: "bulletList",
      },
      {
        name: "orderedList",
        content: {
          type: "doc",
          content: [
            {
              type: "orderedList",
              content: [
                { type: "listItem", content: [{ type: "paragraph", content: [{ type: "text", text: "one" }] }] },
              ],
            },
          ],
        },
        activeType: "orderedList",
      },
      {
        name: "blockquote",
        content: {
          type: "doc",
          content: [
            { type: "blockquote", content: [{ type: "paragraph", content: [{ type: "text", text: "quoted" }] }] },
          ],
        },
        activeType: "blockquote",
      },
      {
        name: "codeBlock",
        content: {
          type: "doc",
          content: [{ type: "codeBlock", attrs: { language: null }, content: [{ type: "text", text: "code" }] }],
        },
        activeType: "codeBlock",
      },
    ];

    for (const testCase of cases) {
      let submitted: Record<string, unknown> | undefined;
      const editor = createEditor(testCase.content, (doc) => {
        submitted = doc;
      });

      try {
        expect(editor.isActive(testCase.activeType), testCase.name).toBe(true);

        expect(dispatchEditorKeydown(editor), testCase.name).toBe(true);
        expect(submitted, testCase.name).toBeUndefined();

        expect(dispatchEditorKeydown(editor), testCase.name).toBe(true);
        expect(submitted, testCase.name).toBeUndefined();
        expect(editor.isActive(testCase.activeType), testCase.name).toBe(false);
        expect(isChatSubmitBoundary(editor), testCase.name).toBe(true);

        expect(dispatchEditorKeydown(editor), testCase.name).toBe(true);
        expect(submitted, testCase.name).toEqual(editor.getJSON());
      } finally {
        editor.destroy();
      }
    }
  });

  test("uses Tiptap Markdown-style input rules instead of the Markdown extension", () => {
    const editor = createEditor({ type: "doc", content: [{ type: "paragraph" }] });

    try {
      const extensionNames = editor.extensionManager.extensions.map((extension) => extension.name);
      expect(extensionNames).toContain("bold");
      expect(extensionNames).toContain("italic");
      expect(extensionNames).toContain("strike");
      expect(extensionNames).toContain("code");
      expect(extensionNames).toContain("codeBlock");
      expect(extensionNames).toContain("blockquote");
      expect(extensionNames).toContain("bulletList");
      expect(extensionNames).toContain("orderedList");
      expect(extensionNames).toContain("listKeymap");
      expect(extensionNames).not.toContain("trailingNode");
      expect(extensionNames).not.toContain("markdown");
      expect(editor.schema.nodes.listItem.spec.content).toBe("paragraph block*");
    } finally {
      editor.destroy();
    }
  });

  test("formats selected pasted multi-line code as one code block", () => {
    const editor = createEditor({
      type: "doc",
      content: [
        { type: "paragraph", content: [{ type: "text", text: "one" }] },
        { type: "paragraph", content: [{ type: "text", text: "two" }] },
      ],
    });

    try {
      editor.commands.setTextSelection({ from: 1, to: editor.state.doc.content.size });

      editor.chain().focus().toggleCodeBlock().run();
      expect(editor.getJSON()).toEqual({
        type: "doc",
        content: [
          {
            type: "codeBlock",
            attrs: { language: null },
            content: [{ type: "text", text: "one\ntwo" }],
          },
        ],
      });
      expect(tiptapToRichMessage(editor.getJSON())).toEqual({
        body: "one\ntwo",
        markup: [{ type: "bcode", start: 0, end: 7 }],
        references: [],
      });
    } finally {
      editor.destroy();
    }
  });

  test("preserves selected hard breaks when formatting as a code block", () => {
    const editor = createEditor({
      type: "doc",
      content: [
        {
          type: "paragraph",
          content: [
            { type: "text", text: "one" },
            { type: "hardBreak" },
            { type: "text", text: "two" },
          ],
        },
      ],
    });

    try {
      editor.commands.setTextSelection({ from: 1, to: editor.state.doc.content.size });

      editor.chain().focus().toggleCodeBlock().run();
      expect(editor.getJSON()).toEqual({
        type: "doc",
        content: [
          {
            type: "codeBlock",
            attrs: { language: null },
            content: [{ type: "text", text: "one\ntwo" }],
          },
        ],
      });
    } finally {
      editor.destroy();
    }
  });

  test("delegates nested selections to Tiptap's normal code block command", () => {
    const editor = createEditor({
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
    });

    try {
      let firstItemParagraphStart: number | null = null;
      editor.state.doc.descendants((node, pos, parent) => {
        if (node.type.name === "paragraph" && parent?.type.name === "listItem" && firstItemParagraphStart === null) {
          firstItemParagraphStart = pos;
          return false;
        }
        return true;
      });
      expect(firstItemParagraphStart).not.toBeNull();
      editor.commands.setTextSelection({
        from: firstItemParagraphStart! + 1,
        to: firstItemParagraphStart! + 4,
      });

      editor.commands.toggleCodeBlock();
      expect(editor.getText()).toContain("two");
    } finally {
      editor.destroy();
    }
  });

  test("keeps normal code block toggling for a cursor inside one block", () => {
    const editor = createEditor({
      type: "doc",
      content: [{ type: "paragraph", content: [{ type: "text", text: "code" }] }],
    });

    try {
      expect(editor.commands.toggleCodeBlock()).toBe(true);
      expect(editor.getJSON()).toEqual({
        type: "doc",
        content: [
          {
            type: "codeBlock",
            attrs: { language: null },
            content: [{ type: "text", text: "code" }],
          },
        ],
      });

      expect(editor.commands.toggleCodeBlock()).toBe(true);
      expect(editor.getJSON()).toEqual({
        type: "doc",
        content: [
          { type: "paragraph", content: [{ type: "text", text: "code" }] },
        ],
      });
    } finally {
      editor.destroy();
    }
  });

  test("serializes TipTap marks and links as XEP body, markup, and references", () => {
    const serialized = tiptapToRichMessage({
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
    });

    expect(serialized.body).toBe("Hi there docs");
    expect(serialized.markup).toEqual([
      { type: "span", start: 3, end: 8, styles: ["emphasis", "strong"] },
    ]);
    expect(serialized.references).toEqual([
      { type: "data", uri: "https://example.com/docs", begin: 8, end: 13 },
    ]);
  });

  test("serializes lists without blank lines between sibling items", () => {
    const serialized = tiptapToRichMessage({
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
    });

    expect(serialized.body).toBe("- one\n- two");
    expect(serialized.markup).toEqual([
      { type: "list", start: 0, end: 11, ordered: false, items: [0, 6] },
    ]);
  });

  test("round-trips nested lists through XEP metadata", () => {
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

  test("hydrates existing XEP list messages as sibling TipTap list items", () => {
    const body = "- one\n- three";
    const markup: MarkupSpan[] = [
      { type: "list", start: 0, end: body.length, ordered: false, items: [0, 6] },
    ];

    const editor = createEditor(richMessageToTiptap({ body, markup }));

    try {
      expect(tiptapToRichMessage(editor.getJSON()).body).toBe(body);
    } finally {
      editor.destroy();
    }
  });

  test("uses code point offsets for emoji", () => {
    expect(tiptapToRichMessage({
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
    }).markup).toEqual([
      { type: "span", start: 2, end: 6, styles: ["strong"] },
    ]);
  });

  test("exports XEP metadata to Markdown without parsing raw bodies as Markdown", () => {
    const body = "bold italic gone code docs\n\nconst x = 1";
    const markup: MarkupSpan[] = [
      { type: "span", start: 0, end: 4, styles: ["strong"] },
      { type: "span", start: 5, end: 11, styles: ["emphasis"] },
      { type: "span", start: 12, end: 16, styles: ["deleted"] },
      { type: "span", start: 17, end: 21, styles: ["code"] },
      { type: "bcode", start: 28, end: body.length, language: "ts" },
    ];

    expect(richMessageToMarkdown({
      body,
      markup,
      references: [{ type: "data", uri: "https://example.com/docs", begin: 22, end: 26 }],
    })).toBe([
      "**bold** *italic* ~~gone~~ `code` [docs](https://example.com/docs)",
      "",
      "```ts",
      "const x = 1",
      "```",
    ].join("\n"));

    expect(richMessageToMarkdown({ body: "raw **not bold**" })).toBe("raw \\*\\*not bold\\*\\*");
  });
});
