// Codec round-trip for the wasm → TS feed entry boundary, focused on
// the PEP-bridge `source` discriminator added in the PEP → community
// feed bridge. Manual posts must round-trip without a `source` field;
// bridged entries must carry the kind through; unknown kinds must be
// silently dropped (forward-compat for new bridge kinds the server
// may emit before the client knows about them).

import { describe, expect, test } from "bun:test";
import { feedEntryFromWasm, type WasmFeedEntry } from "@/lib/xmpp/feed-types";

describe("feedEntryFromWasm", () => {
  test("manual post round-trips without a source field", () => {
    const wasm: WasmFeedEntry = {
      id: "post-1",
      body: "Hello world",
      author: "alice@localhost",
      published: "2026-05-16T12:00:00Z",
    };
    const entry = feedEntryFromWasm(wasm);
    expect(entry.id).toBe("post-1");
    expect(entry.body).toBe("Hello world");
    expect(entry.author).toBe("alice@localhost");
    expect(entry.publishedMs).toBe(Date.parse("2026-05-16T12:00:00Z"));
    expect(entry.source).toBeUndefined();
  });

  test("bridged entry surfaces the source kind", () => {
    for (const kind of ["mood", "activity", "tune", "avatar", "vcard", "rsvp"] as const) {
      const wasm: WasmFeedEntry = {
        id: `bridged-${kind}`,
        body: "summary",
        author: "alice@localhost",
        source: kind,
      };
      expect(feedEntryFromWasm(wasm).source).toBe(kind);
    }
  });

  test("unknown source kind is dropped (forward-compat)", () => {
    const wasm: WasmFeedEntry = {
      id: "bridged-future",
      body: "summary",
      author: "alice@localhost",
      source: "telemetry",
    };
    expect(feedEntryFromWasm(wasm).source).toBeUndefined();
  });

  test("null source is treated as absent", () => {
    const wasm: WasmFeedEntry = {
      id: "post-null",
      body: "summary",
      source: null,
    };
    expect(feedEntryFromWasm(wasm).source).toBeUndefined();
  });
});
