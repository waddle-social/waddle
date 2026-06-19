import { describe, expect, test } from "bun:test";
import {
  backgroundCatalog,
  catalogEntry,
} from "../src/lib/calls/background-effect/backgrounds";

describe("background catalog", () => {
  test("lists every catalog image with a same-origin self-hosted asset path", () => {
    const entries = backgroundCatalog();

    expect(entries.length).toBeGreaterThan(0);
    for (const entry of entries) {
      expect(entry.label.length).toBeGreaterThan(0);
      // Self-hosted: a root-relative path, never a third-party CDN URL.
      expect(entry.assetPath.startsWith("/")).toBe(true);
      expect(entry.assetPath).not.toContain("//");
    }
  });

  test("catalogEntry matches the catalog entry for every id (no drift)", () => {
    // backgroundCatalog() and catalogEntry() duplicate the label/path literals;
    // pin that they agree for EVERY id so one can't silently drift from the other.
    for (const entry of backgroundCatalog()) {
      expect(catalogEntry(entry.id)).toEqual(entry);
    }
  });
});
