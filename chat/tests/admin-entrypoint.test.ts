import { describe, expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";

describe("admin Astro entrypoints", () => {
  test("serves both /admin and /admin/:panel through the admin route shell", () => {
    const rootPage = new URL("../src/pages/admin/index.astro", import.meta.url);
    const panelPage = new URL("../src/pages/admin/[panel].astro", import.meta.url);

    expect(existsSync(rootPage)).toBe(true);
    expect(existsSync(panelPage)).toBe(true);

    for (const page of [rootPage, panelPage]) {
      const source = readFileSync(page, "utf8");
      expect(source).toContain('from "@/layouts/AppLayout.astro"');
      expect(source).toContain('from "@/components/pages/RoutePageShell.vue"');
      expect(source).toContain('routeId="admin"');
    }
  });
});
