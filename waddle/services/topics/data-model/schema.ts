import { sql } from "drizzle-orm";
import { integer, sqliteTable, text, uniqueIndex, check } from "drizzle-orm/sqlite-core";

export const topics = sqliteTable(
  "topics",
  {
    id: text("id").primaryKey(),
    title: text("title").notNull(),
    description: text("description"),
    scope: text("scope", { enum: ["global", "owner", "waddle"] }).notNull(),
    ownerId: text("owner_id"),
    waddleSlug: text("waddle_slug"),
    createdAt: integer("created_at", { mode: "number" })
      .notNull()
      .default(sql`(strftime('%s', 'now') * 1000)`),
    updatedAt: integer("updated_at", { mode: "number" })
      .notNull()
      .default(sql`(strftime('%s', 'now') * 1000)`),
  },
  (table) => ({
    ownerScopeCheck: check(
      "topics_owner_scope_check",
      sql`${table.scope} != 'owner' OR (${table.ownerId} IS NOT NULL AND ${table.waddleSlug} IS NULL)`,
    ),
    waddleScopeCheck: check(
      "topics_waddle_scope_check",
      sql`${table.scope} != 'waddle' OR (${table.waddleSlug} IS NOT NULL AND ${table.ownerId} IS NULL)`,
    ),
    globalScopeCheck: check(
      "topics_global_scope_check",
      sql`${table.scope} != 'global' OR (${table.ownerId} IS NULL AND ${table.waddleSlug} IS NULL)`,
    ),
    ownerTitleUnique: uniqueIndex("topics_owner_title_unique")
      .on(table.ownerId, table.title)
      .where(sql`${table.scope} = 'owner'`),
    waddleTitleUnique: uniqueIndex("topics_waddle_title_unique")
      .on(table.waddleSlug, table.title)
      .where(sql`${table.scope} = 'waddle'`),
  }),
);
