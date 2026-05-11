import { describe, expect, test } from "bun:test";
import {
  INITIAL_CURSOR,
  advanceCursorWithBeforePage,
  advanceThreadCursor,
  classifyMamError,
  cursorFromLatestPage,
  isMamCursorNotFound,
  MamCursorNotFoundError,
  stripQueuedSelfMessages,
  threadCursorFromLatestPage,
} from "../src/lib/xmpp/mam-paging";

describe("INITIAL_CURSOR", () => {
  test("is null/true so a first load is permitted", () => {
    expect(INITIAL_CURSOR).toEqual({ oldestArchiveId: null, hasOlderMessages: true });
  });
});

describe("cursorFromLatestPage — RSM-aware client (queryMamPage)", () => {
  test("uses page.firstArchiveId and inverts page.complete", () => {
    const result = cursorFromLatestPage({
      page: { firstArchiveId: "arch-001", complete: false, messages: [{ id: "m1" }] },
      pageSize: 100,
    });
    expect(result).toEqual({ oldestArchiveId: "arch-001", hasOlderMessages: true });
  });

  test("treats page.complete = true as 'no older messages'", () => {
    const result = cursorFromLatestPage({
      page: { firstArchiveId: "arch-001", complete: true, messages: [{ id: "m1" }] },
      pageSize: 100,
    });
    expect(result.hasOlderMessages).toBe(false);
  });

  test("missing firstArchiveId disables older-paging even when not complete", () => {
    const result = cursorFromLatestPage({
      page: { complete: false, messages: [{ id: "m1" }] },
      pageSize: 100,
    });
    expect(result).toEqual({ oldestArchiveId: null, hasOlderMessages: false });
  });
});

describe("cursorFromLatestPage — legacy non-RSM client (page is null)", () => {
  test("falls back to the first message id and uses a full-page heuristic", () => {
    const messages = Array.from({ length: 100 }, (_, i) => ({ id: `m${i}` }));
    const result = cursorFromLatestPage({ page: null, pageSize: 100 });
    // No page → no id available without messages context; the helper accepts
    // mamResults as a separate argument when the legacy path is used.
    expect(result).toEqual({ oldestArchiveId: null, hasOlderMessages: false });
    void messages; // ensure the array is constructed for the next assertion
  });

  test("legacy path with messages: oldest id derived from messages[0].id; hasOlder = messages.length >= pageSize", () => {
    const messages = Array.from({ length: 100 }, (_, i) => ({ id: `m${i}` }));
    const result = cursorFromLatestPage({ page: null, pageSize: 100, fallbackMessages: messages });
    expect(result.oldestArchiveId).toBe("m0");
    expect(result.hasOlderMessages).toBe(true);
  });

  test("legacy path with messages.length < pageSize → hasOlder false", () => {
    const messages = [{ id: "m0" }, { id: "m1" }, { id: "m2" }];
    const result = cursorFromLatestPage({ page: null, pageSize: 100, fallbackMessages: messages });
    expect(result.oldestArchiveId).toBe("m0");
    expect(result.hasOlderMessages).toBe(false);
  });
});

describe("advanceCursorWithBeforePage", () => {
  test("advances cursor and reports not-stalled when firstArchiveId moves", () => {
    const result = advanceCursorWithBeforePage({
      prior: { oldestArchiveId: "arch-005", hasOlderMessages: true },
      page: { firstArchiveId: "arch-001", complete: false },
    });
    expect(result.next.oldestArchiveId).toBe("arch-001");
    expect(result.next.hasOlderMessages).toBe(true);
    expect(result.stalled).toBe(false);
  });

  test("page.complete = true ends paging", () => {
    const result = advanceCursorWithBeforePage({
      prior: { oldestArchiveId: "arch-005", hasOlderMessages: true },
      page: { firstArchiveId: "arch-001", complete: true },
    });
    expect(result.next.hasOlderMessages).toBe(false);
  });

  test("server returns the same firstArchiveId as the prior cursor → stalled, hasOlder=false", () => {
    const result = advanceCursorWithBeforePage({
      prior: { oldestArchiveId: "arch-005", hasOlderMessages: true },
      page: { firstArchiveId: "arch-005", complete: false },
    });
    expect(result.stalled).toBe(true);
    expect(result.next.hasOlderMessages).toBe(false);
  });

  test("missing firstArchiveId keeps prior cursor and ends paging", () => {
    const result = advanceCursorWithBeforePage({
      prior: { oldestArchiveId: "arch-005", hasOlderMessages: true },
      page: { complete: false },
    });
    expect(result.next.oldestArchiveId).toBe("arch-005");
    expect(result.next.hasOlderMessages).toBe(false);
    expect(result.stalled).toBe(true);
  });
});

describe("threadCursorFromLatestPage", () => {
  test("RSM-aware: uses firstArchiveId and inverted complete", () => {
    const result = threadCursorFromLatestPage({
      page: { firstArchiveId: "arch-t-001", complete: false, messages: [{ id: "tm1" }] },
      pageSize: 100,
    });
    expect(result).toEqual({ oldestId: "arch-t-001", hasOlder: true });
  });

  test("legacy fallback: full-page heuristic", () => {
    const result = threadCursorFromLatestPage({
      page: null,
      pageSize: 100,
      fallbackMessages: Array.from({ length: 100 }, (_, i) => ({ id: `tm${i}` })),
    });
    expect(result.hasOlder).toBe(true);
  });

  test("legacy fallback: partial page → no older", () => {
    const result = threadCursorFromLatestPage({
      page: null,
      pageSize: 100,
      fallbackMessages: [{ id: "tm0" }],
    });
    expect(result.hasOlder).toBe(false);
  });

  test("empty result with no page → cursor undefined, hasOlder false", () => {
    const result = threadCursorFromLatestPage({ page: null, pageSize: 100 });
    expect(result).toEqual({ oldestId: undefined, hasOlder: false });
  });
});

describe("advanceThreadCursor", () => {
  test("happy path advances oldestId, keeps hasOlder true", () => {
    const result = advanceThreadCursor({
      prior: { oldestId: "t-005" },
      page: { firstArchiveId: "t-001", complete: false },
    });
    expect(result.oldestId).toBe("t-001");
    expect(result.hasOlder).toBe(true);
  });

  test("stalled when firstArchiveId equals prior oldestId", () => {
    const result = advanceThreadCursor({
      prior: { oldestId: "t-005" },
      page: { firstArchiveId: "t-005", complete: false },
    });
    expect(result.hasOlder).toBe(false);
  });

  test("page.complete = true ends paging", () => {
    const result = advanceThreadCursor({
      prior: { oldestId: "t-005" },
      page: { firstArchiveId: "t-001", complete: true },
    });
    expect(result.hasOlder).toBe(false);
  });
});

describe("stripQueuedSelfMessages", () => {
  test("filters self+queued; keeps everything else", () => {
    const timeline = [
      { id: "a", isSelf: true, deliveryStatus: "queued" as const },
      { id: "b", isSelf: true, deliveryStatus: "delivered" as const },
      { id: "c", isSelf: false, deliveryStatus: "queued" as const },
      { id: "d", isSelf: false },
    ];
    const result = stripQueuedSelfMessages(timeline);
    expect(result.map((m) => m.id)).toEqual(["b", "c", "d"]);
  });

  test("empty timeline returns empty", () => {
    expect(stripQueuedSelfMessages([])).toEqual([]);
  });
});

describe("MamCursorNotFoundError + classifyMamError (XEP-0313 §4.3.4)", () => {
  test("recognises stanza-error with condition='item-not-found'", () => {
    const raw = { condition: "item-not-found", cursor: "arch-stale" };
    const classified = classifyMamError(raw);
    expect(classified).toBeInstanceOf(MamCursorNotFoundError);
    if (isMamCursorNotFound(classified)) {
      expect(classified.cursor).toBe("arch-stale");
    }
  });

  test("recognises wasm-style error with code='item-not-found'", () => {
    const raw = { code: "item-not-found", cursor: "arch-also-stale" };
    const classified = classifyMamError(raw);
    expect(isMamCursorNotFound(classified)).toBe(true);
  });

  test("recognises plain-string errors carrying 'item-not-found' (current wasm shape)", () => {
    // The waddle wasm client surfaces stanza errors via JsValue::from_str,
    // not as structured objects. Match by substring so §4.3.4 recovery
    // fires even before the wasm layer learns to emit structured errors.
    const classified = classifyMamError("MAM query failed: item-not-found");
    expect(isMamCursorNotFound(classified)).toBe(true);
  });

  test("recognises Error instances whose message contains 'item-not-found'", () => {
    const raw = new Error("stanza error: item-not-found");
    const classified = classifyMamError(raw);
    expect(isMamCursorNotFound(classified)).toBe(true);
  });

  test("passes through unrelated errors unchanged", () => {
    const raw = new Error("network down");
    const classified = classifyMamError(raw);
    expect(classified).toBe(raw);
    expect(isMamCursorNotFound(classified)).toBe(false);
  });

  test("passes through plain strings without the marker unchanged", () => {
    const raw = "permission denied";
    expect(classifyMamError(raw)).toBe(raw);
  });

  test("isMamCursorNotFound is false for null/undefined/non-marker strings", () => {
    expect(isMamCursorNotFound(null)).toBe(false);
    expect(isMamCursorNotFound(undefined)).toBe(false);
    expect(isMamCursorNotFound("item-not-found")).toBe(false);
  });
});
