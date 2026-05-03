import { describe, test, expect } from "bun:test";
import { buildReplyFallbackPrefix } from "../src/lib/xmpp/send-types";
import { codePointLength } from "../src/lib/text-offsets";

describe("buildReplyFallbackPrefix", () => {
  test("returns empty values when parent body is missing", () => {
    expect(buildReplyFallbackPrefix(undefined)).toEqual({ prefix: "", length: 0 });
    expect(buildReplyFallbackPrefix("")).toEqual({ prefix: "", length: 0 });
  });

  test("quotes a single line and appends a blank line", () => {
    const { prefix, length } = buildReplyFallbackPrefix("hello");
    expect(prefix).toBe("> hello\n\n");
    expect(length).toBe(prefix.length);
  });

  test("quotes each line of a multi-line parent body", () => {
    const { prefix, length } = buildReplyFallbackPrefix("line one\nline two\nline three");
    expect(prefix).toBe("> line one\n> line two\n> line three\n\n");
    expect(length).toBe(prefix.length);
  });

  test("length matches the returned prefix character count", () => {
    const parent = "hi 👋\nmultiline";
    const { prefix, length } = buildReplyFallbackPrefix(parent);
    expect(length).toBe(codePointLength(prefix));
    expect(prefix.startsWith("> hi ")).toBe(true);
    expect(prefix.endsWith("\n\n")).toBe(true);
  });
});
