import { roomJidFromJoinUri } from "@/lib/room-links";
import StarterKit from "@tiptap/starter-kit";
import Placeholder from "@tiptap/extension-placeholder";
import type { Extensions, JSONContent } from "@tiptap/core";
import { ChatCodeBlock } from "@/lib/editor/chat-code-block";
import { ChatLink } from "@/lib/editor/chat-link";
import { ChatKeymap } from "@/lib/editor/chat-keymap";
import { css } from "styled-system/css";

const linkClass = css({
  color: "action.text",
  textDecoration: "underline",
  textDecorationColor: "color-mix(in oklab, currentColor 40%, transparent)",
  transition: "text-decoration-color 0.15s",
  _hover: { textDecorationColor: "currentColor" },
});

interface ChatEditorExtensionsOptions {
  placeholder?: string | (() => string);
  includePlaceholder?: boolean;
  onSubmit?: (doc: JSONContent) => boolean | void;
}

export function createChatEditorExtensions(options: ChatEditorExtensionsOptions = {}): Extensions {
  const extensions: Extensions = [];

  if (options.onSubmit) {
    extensions.push(ChatKeymap.configure({ onSubmit: options.onSubmit }));
  }

  extensions.push(
    StarterKit.configure({
      codeBlock: false,
      heading: false,
      horizontalRule: false,
      link: false,
      trailingNode: false,
      underline: false,
    }),
    ChatCodeBlock.configure({
      exitOnTripleEnter: false,
    }),
    ChatLink.configure({
      openOnClick: false,
      isAllowedUri: (uri, ctx) => !!roomJidFromJoinUri(uri) || ctx.defaultValidate(uri),
      autolink: true,
      linkOnPaste: true,
      HTMLAttributes: {
        class: linkClass,
      },
    }),
  );

  if (options.includePlaceholder ?? true) {
    extensions.push(
      Placeholder.configure({
        placeholder: options.placeholder ?? "",
      }),
    );
  }

  return extensions;
}
