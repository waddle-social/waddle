import { sql } from "drizzle-orm";
import { integer, sqliteTable, text, uniqueIndex } from "drizzle-orm/sqlite-core";

export const waitlistStateValues = ["pending", "confirmed"] as const;
export const waitlistTokenKindValues = ["confirm", "unsubscribe"] as const;

export type WaitlistState = (typeof waitlistStateValues)[number];
export type WaitlistTokenKind = (typeof waitlistTokenKindValues)[number];

export const waitlistEntries = sqliteTable(
	"waitlist_entries",
	{
		id: text("id").primaryKey(),
		email: text("email").notNull(),
		state: text("state", { enum: waitlistStateValues }).default("pending").notNull(),
		confirmedAt: integer("confirmed_at", { mode: "timestamp_ms" }),
		createdAt: integer("created_at", { mode: "timestamp_ms" })
			.default(sql`(cast(unixepoch('subsecond') * 1000 as integer))`)
			.notNull(),
		updatedAt: integer("updated_at", { mode: "timestamp_ms" })
			.default(sql`(cast(unixepoch('subsecond') * 1000 as integer))`)
			.$onUpdate(() => /* @__PURE__ */ new Date())
			.notNull(),
	},
	(table) => [uniqueIndex("waitlist_entries_email_unique").on(table.email)],
);

export const waitlistTokens = sqliteTable(
	"waitlist_tokens",
	{
		id: text("id").primaryKey(),
		entryId: text("entry_id")
			.notNull()
			.references(() => waitlistEntries.id, { onDelete: "cascade" }),
		kind: text("kind", { enum: waitlistTokenKindValues }).notNull(),
		tokenHash: text("token_hash").notNull(),
		consumedAt: integer("consumed_at", { mode: "timestamp_ms" }),
		createdAt: integer("created_at", { mode: "timestamp_ms" })
			.default(sql`(cast(unixepoch('subsecond') * 1000 as integer))`)
			.notNull(),
	},
	(table) => [
		uniqueIndex("waitlist_tokens_hash_unique").on(table.tokenHash),
		uniqueIndex("waitlist_tokens_entry_kind_unique").on(table.entryId, table.kind),
	],
);
