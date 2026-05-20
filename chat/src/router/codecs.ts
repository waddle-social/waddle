import type { SearchCodec } from "./define";

function stripLeadingQuestion(s: string): string {
  return s.startsWith("?") ? s.slice(1) : s;
}

// Decode a `thread=` query value into a list of ids.
//
// Decoding goes through a regex against the *raw* query rather than
// `URLSearchParams.get()` because the latter already decodes percent
// escapes — and ids that contain a literal `,` or `%` rely on a single
// pass of `decodeURIComponent` after we split. A double-decode would
// turn `%252C` (an id containing a literal `%2C`) back into a separator
// comma, then split incorrectly.
function decodeThreadStack(query: string): string[] {
  const match = /(?:^|&)thread=([^&]*)/.exec(query);
  const raw = match?.[1];
  if (!raw) return [];
  return raw
    .split(",")
    .map((s) => {
      const trimmed = s.trim();
      try {
        return decodeURIComponent(trimmed);
      } catch {
        return trimmed;
      }
    })
    .filter((s) => s.length > 0);
}

function encodeThreadStack(stack: readonly string[]): string | null {
  if (stack.length === 0) return null;
  return `thread=${stack.map((id) => encodeURIComponent(id)).join(",")}`;
}

export const threadSearch: SearchCodec<{ thread: string[] }> = {
  encode: ({ thread }) => {
    const encoded = encodeThreadStack(thread);
    return encoded ? `?${encoded}` : "";
  },
  decode: (searchString) => {
    if (!searchString) return { thread: [] };
    return { thread: decodeThreadStack(stripLeadingQuestion(searchString)) };
  },
};

export const threadAndPinnedSearch: SearchCodec<{ thread: string[]; pinned: boolean }> = {
  encode: ({ thread, pinned }) => {
    const parts: string[] = [];
    const stack = encodeThreadStack(thread);
    if (stack) parts.push(stack);
    if (pinned) parts.push("pinned=1");
    return parts.length > 0 ? `?${parts.join("&")}` : "";
  },
  decode: (searchString) => {
    if (!searchString) return { thread: [], pinned: false };
    const q = stripLeadingQuestion(searchString);
    return {
      thread: decodeThreadStack(q),
      pinned: /(?:^|&)pinned=1(?:&|$)/.test(q),
    };
  },
};
