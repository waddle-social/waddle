import { sqliteTable, text } from "drizzle-orm/sqlite-core";

export const example = sqliteTable("example", {
	id: text("id").primaryKey(),
});

// TODO: Replace with your own tables and relations.
