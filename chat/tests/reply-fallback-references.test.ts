import { describe, expect, test } from "bun:test";
import {
  buildReplyFallbackPrefix,
  shiftReferenceOffsets,
} from "@/lib/xmpp/send-types";

describe("shiftReferenceOffsets (XEP-0461 reply-fallback rebasing)", () => {
  test("shifts every reference begin/end by the prefix length", () => {
    const { length } = buildReplyFallbackPrefix("hello world");
    expect(length).toBeGreaterThan(0);

    const shifted = shiftReferenceOffsets(
      [
        { type: "data", uri: "https://foo", begin: 0, end: 11 },
        { type: "mention", uri: "xmpp:bob@example.com", begin: 4, end: 8 },
      ],
      length,
    );

    expect(shifted).toEqual([
      { type: "data", uri: "https://foo", begin: length, end: length + 11 },
      { type: "mention", uri: "xmpp:bob@example.com", begin: length + 4, end: length + 8 },
    ]);
  });

  test("drops references with non-numeric or invalid offsets", () => {
    const shifted = shiftReferenceOffsets(
      [
        { type: "data", uri: "https://foo", begin: undefined, end: 5 },
        { type: "data", uri: "https://bar", begin: 5, end: 5 },
        { type: "data", uri: "https://baz", begin: -1, end: 3 },
        { type: "data", uri: "https://ok", begin: 0, end: 4 },
      ],
      10,
    );

    expect(shifted).toEqual([
      { type: "data", uri: "https://ok", begin: 10, end: 14 },
    ]);
  });

  test("zero-length prefix is a no-op", () => {
    const refs = [{ type: "data", uri: "https://foo", begin: 4, end: 23 }];
    expect(shiftReferenceOffsets(refs, 0)).toEqual(refs);
  });
});
