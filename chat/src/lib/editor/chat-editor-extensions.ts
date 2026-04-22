import StarterKit from "@tiptap/starter-kit";
import Placeholder from "@tiptap/extension-placeholder";
import type { Extensions, JSONContent } from "@tiptap/core";
import { ChatLink } from "@/lib/editor/chat-link";
import { ChatKeymap } from "@/lib/editor/chat-keymap";

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
      codeBlock: {
        exitOnTripleEnter: false,
      },
      heading: false,
      horizontalRule: false,
      link: false,
      trailingNode: false,
      underline: false,
    }),
    ChatLink.configure({
      openOnClick: false,
      autolink: true,
      linkOnPaste: true,
      HTMLAttributes: {
        class: "text-primary underline decoration-primary/40 hover:decoration-primary transition-colors",
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
