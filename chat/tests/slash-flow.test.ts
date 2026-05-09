import { describe, expect, test } from "bun:test";
import { Editor } from "@tiptap/core";
import { createChatEditorExtensions } from "../src/lib/editor/chat-editor-extensions";
import { parseSlashTrigger } from "../src/lib/slash-trigger";
import { filterSlashCandidates, resolveSlashCommand } from "../src/lib/slash-match";
import { buildSlashInvocation } from "../src/lib/slash-dispatch";
import type { DiscoveredExtensionCommand } from "../src/lib/xmpp/extension-commands";

if (typeof globalThis.requestAnimationFrame !== "function") {
  globalThis.requestAnimationFrame = ((callback: FrameRequestCallback) => {
    callback(performance.now());
    return 0;
  }) as typeof globalThis.requestAnimationFrame;
}

const ai: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:ai-chatbot",
  name: "Ask AI Chatbot",
  scope: "global",
  composerPrefix: "ai",
  inlineField: "prompt",
};

const poll: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:decision-polls",
  name: "Create Decision Poll",
  scope: "channel",
  composerPrefix: "poll",
};

function createEditor(text: string, secondParagraph?: string): Editor {
  const content: Record<string, unknown> = {
    type: "doc",
    content: [
      {
        type: "paragraph",
        content: text.length > 0 ? [{ type: "text", text }] : [],
      },
      ...(secondParagraph !== undefined
        ? [
            {
              type: "paragraph",
              content: secondParagraph.length > 0 ? [{ type: "text", text: secondParagraph }] : [],
            },
          ]
        : []),
    ],
  };
  return new Editor({
    enableCoreExtensions: { keymap: false },
    extensions: createChatEditorExtensions({
      includePlaceholder: false,
      onSubmit: () => false,
    }),
    content,
  });
}

function firstParagraphText(editor: Editor): string {
  const firstChild = editor.state.doc.firstChild;
  if (!firstChild || firstChild.type.name !== "paragraph") return "";
  return firstChild.textContent;
}

describe("slash command flow against a live TipTap editor", () => {
  test("`/ai hello world` resolves and dispatches as inline-submit", () => {
    const editor = createEditor("/ai hello world");

    const text = firstParagraphText(editor);
    const trigger = parseSlashTrigger(text);
    expect(trigger).toEqual({ prefix: "ai", trailing: "hello world" });

    const matched = resolveSlashCommand(trigger!.prefix, [ai, poll], { inMuc: false });
    expect(matched).toBe(ai);

    const invocation = buildSlashInvocation(matched!, trigger!.trailing);
    expect(invocation).toEqual({
      kind: "inline-submit",
      command: ai,
      fieldName: "prompt",
      value: "hello world",
    });
  });

  test("`/poll Best mascot?` in a MUC opens the palette with first-required prefill", () => {
    const editor = createEditor("/poll Best mascot?");

    const text = firstParagraphText(editor);
    const trigger = parseSlashTrigger(text);
    expect(trigger).toEqual({ prefix: "poll", trailing: "Best mascot?" });

    const matched = resolveSlashCommand(trigger!.prefix, [ai, poll], { inMuc: true });
    expect(matched).toBe(poll);

    const invocation = buildSlashInvocation(matched!, trigger!.trailing);
    expect(invocation).toEqual({
      kind: "open-palette",
      command: poll,
      prefillFirstRequired: "Best mascot?",
    });
  });

  test("`/poll` outside a MUC shows no candidates (channel-scope filter)", () => {
    const editor = createEditor("/poll");

    const text = firstParagraphText(editor);
    const trigger = parseSlashTrigger(text);
    expect(trigger).toEqual({ prefix: "poll", trailing: "" });

    expect(filterSlashCandidates(trigger!.prefix, [ai, poll], { inMuc: false })).toEqual([]);
    expect(resolveSlashCommand(trigger!.prefix, [ai, poll], { inMuc: false })).toBeNull();
  });

  test("`/xyz` blocks: no candidates, no resolution", () => {
    const editor = createEditor("/xyz");

    const text = firstParagraphText(editor);
    const trigger = parseSlashTrigger(text);
    expect(trigger).toEqual({ prefix: "xyz", trailing: "" });

    expect(filterSlashCandidates(trigger!.prefix, [ai, poll], { inMuc: true })).toEqual([]);
    expect(resolveSlashCommand(trigger!.prefix, [ai, poll], { inMuc: true })).toBeNull();
  });

  test("plain text `hello` produces no slash trigger", () => {
    const editor = createEditor("hello");

    const text = firstParagraphText(editor);
    expect(parseSlashTrigger(text)).toBeNull();
  });

  test("`/ai` in the first paragraph does not pull trailing text from a second paragraph", () => {
    const editor = createEditor("/ai", "hello");

    // Trigger looks at the first paragraph only; the second paragraph's text
    // is not pulled into `trailing`.
    const text = firstParagraphText(editor);
    const trigger = parseSlashTrigger(text);
    expect(trigger).toEqual({ prefix: "ai", trailing: "" });
  });
});
