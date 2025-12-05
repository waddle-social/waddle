import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const migrationPath = join(
  __dirname,
  "../migrations/0000_lyrical_karen_page.sql",
);

const migrationSql = readFileSync(migrationPath, "utf-8");

describe("topics migration", () => {
  it("creates topics table with scope constraints", () => {
    expect(migrationSql).toContain("CREATE TABLE `topics`");
    expect(migrationSql).toContain("topics_owner_scope_check");
    expect(migrationSql).toContain("topics_waddle_scope_check");
    expect(migrationSql).toContain("topics_global_scope_check");
  });

  it("indexes titles per scope", () => {
    expect(migrationSql).toContain("topics_owner_title_unique");
    expect(migrationSql).toContain("topics_waddle_title_unique");
  });
});
