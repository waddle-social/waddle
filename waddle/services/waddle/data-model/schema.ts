import { sqliteTable, text } from "drizzle-orm/sqlite-core";

export const waddle = sqliteTable("waddles", {
  slug: text("slug").primaryKey(),
  name: text("name").notNull(),
  visibility: text("visibility", { enum: ["public", "private", "secret"] })
    .notNull()
    .default("public"),
});
