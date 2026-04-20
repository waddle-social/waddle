import Link from "@tiptap/extension-link";

export const ChatLink = Link.extend({
  inclusive() {
    // Keep pasted/autolinked URLs from absorbing text typed at the mark boundary.
    return false;
  },
});
