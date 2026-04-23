import { and, eq } from "drizzle-orm";
import type { DrizzleD1Database } from "drizzle-orm/d1";

import { waitlistEntries, waitlistTokens, type WaitlistState } from "../../db/schema";

type WaitlistDatabase = DrizzleD1Database<Record<string, never>>;

export type ConfirmTokenLookup = {
	tokenId: string;
	entryId: string;
	entryState: WaitlistState;
	consumedAt: Date | null;
};

export type UnsubscribeTokenLookup = {
	tokenId: string;
	entryId: string;
};

export type WaitlistStore = {
	reserveEntry(input: {
		email: string;
		confirmTokenHash: string;
		unsubscribeTokenHash: string;
	}): Promise<{ kind: "created"; entryId: string } | { kind: "duplicate" }>;
	deleteEntry(entryId: string): Promise<void>;
	findConfirmToken(tokenHash: string): Promise<ConfirmTokenLookup | null>;
	findUnsubscribeToken(tokenHash: string): Promise<UnsubscribeTokenLookup | null>;
	confirmEntry(entryId: string, confirmedAt: Date): Promise<void>;
	consumeToken(tokenId: string, consumedAt: Date): Promise<void>;
};

export function createDrizzleWaitlistStore(db: WaitlistDatabase): WaitlistStore {
	return {
		async reserveEntry(input) {
			const entryId = crypto.randomUUID();
			const confirmTokenId = crypto.randomUUID();
			const unsubscribeTokenId = crypto.randomUUID();
			const existing = await db
				.select({ id: waitlistEntries.id })
				.from(waitlistEntries)
				.where(eq(waitlistEntries.email, input.email))
				.limit(1);

			if (existing[0]) {
				return { kind: "duplicate" };
			}

			try {
				await db.insert(waitlistEntries).values({
					id: entryId,
					email: input.email,
					state: "pending",
				});

				await db.insert(waitlistTokens).values([
					{
						id: confirmTokenId,
						entryId,
						kind: "confirm",
						tokenHash: input.confirmTokenHash,
					},
					{
						id: unsubscribeTokenId,
						entryId,
						kind: "unsubscribe",
						tokenHash: input.unsubscribeTokenHash,
					},
				]);

				return { kind: "created", entryId };
			} catch (error) {
				await db.delete(waitlistEntries).where(eq(waitlistEntries.id, entryId)).catch(() => {});
				throw error;
			}
		},

		async deleteEntry(entryId) {
			await db.delete(waitlistEntries).where(eq(waitlistEntries.id, entryId));
		},

		async findConfirmToken(tokenHash) {
			const rows = await db
				.select({
					tokenId: waitlistTokens.id,
					entryId: waitlistEntries.id,
					entryState: waitlistEntries.state,
					consumedAt: waitlistTokens.consumedAt,
				})
				.from(waitlistTokens)
				.innerJoin(waitlistEntries, eq(waitlistEntries.id, waitlistTokens.entryId))
				.where(and(eq(waitlistTokens.kind, "confirm"), eq(waitlistTokens.tokenHash, tokenHash)))
				.limit(1);

			return rows[0] ?? null;
		},

		async findUnsubscribeToken(tokenHash) {
			const rows = await db
				.select({
					tokenId: waitlistTokens.id,
					entryId: waitlistEntries.id,
				})
				.from(waitlistTokens)
				.innerJoin(waitlistEntries, eq(waitlistEntries.id, waitlistTokens.entryId))
				.where(
					and(
						eq(waitlistTokens.kind, "unsubscribe"),
						eq(waitlistTokens.tokenHash, tokenHash),
					),
				)
				.limit(1);

			return rows[0] ?? null;
		},

		async confirmEntry(entryId, confirmedAt) {
			await db
				.update(waitlistEntries)
				.set({
					state: "confirmed",
					confirmedAt,
					updatedAt: confirmedAt,
				})
				.where(eq(waitlistEntries.id, entryId));
		},

		async consumeToken(tokenId, consumedAt) {
			await db
				.update(waitlistTokens)
				.set({
					consumedAt,
				})
				.where(eq(waitlistTokens.id, tokenId));
		},
	};
}
