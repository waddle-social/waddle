import { describe, expect, it } from "bun:test";

import { createOpaqueToken, hashOpaqueToken } from "./crypto";
import {
	confirmWaitlist,
	inspectConfirmToken,
	inspectUnsubscribeToken,
	submitWaitlist,
	unsubscribeWaitlist,
} from "./service";

import type { WaitlistMailer } from "./mailer";
import type { ConfirmTokenLookup, UnsubscribeTokenLookup, WaitlistStore } from "./repository";

class FakeStore implements WaitlistStore {
	private entryCounter = 0;
	private tokenCounter = 0;
	private entries = new Map<string, { id: string; email: string; state: "pending" | "confirmed" }>();
	private confirmTokens = new Map<string, ConfirmTokenLookup>();
	private unsubscribeTokens = new Map<string, UnsubscribeTokenLookup>();

	public deletedEntryIds: string[] = [];
	public confirmedEntryIds: string[] = [];
	public consumedTokenIds: string[] = [];

	async reserveEntry(input: {
		email: string;
		confirmTokenHash: string;
		unsubscribeTokenHash: string;
	}): Promise<{ kind: "created"; entryId: string } | { kind: "duplicate" }> {
		if (this.entries.has(input.email)) {
			return { kind: "duplicate" };
		}

		const entryId = `entry-${++this.entryCounter}`;
		const confirmTokenId = `confirm-${++this.tokenCounter}`;
		const unsubscribeTokenId = `unsubscribe-${++this.tokenCounter}`;

		this.entries.set(input.email, {
			id: entryId,
			email: input.email,
			state: "pending",
		});
		this.confirmTokens.set(input.confirmTokenHash, {
			tokenId: confirmTokenId,
			entryId,
			entryState: "pending",
			consumedAt: null,
		});
		this.unsubscribeTokens.set(input.unsubscribeTokenHash, {
			tokenId: unsubscribeTokenId,
			entryId,
		});

		return { kind: "created", entryId };
	}

	async deleteEntry(entryId: string): Promise<void> {
		this.deletedEntryIds.push(entryId);

		for (const [email, entry] of this.entries.entries()) {
			if (entry.id === entryId) {
				this.entries.delete(email);
			}
		}

		for (const [tokenHash, token] of this.confirmTokens.entries()) {
			if (token.entryId === entryId) {
				this.confirmTokens.delete(tokenHash);
			}
		}

		for (const [tokenHash, token] of this.unsubscribeTokens.entries()) {
			if (token.entryId === entryId) {
				this.unsubscribeTokens.delete(tokenHash);
			}
		}
	}

	async findConfirmToken(tokenHash: string): Promise<ConfirmTokenLookup | null> {
		return this.confirmTokens.get(tokenHash) ?? null;
	}

	async findUnsubscribeToken(tokenHash: string): Promise<UnsubscribeTokenLookup | null> {
		return this.unsubscribeTokens.get(tokenHash) ?? null;
	}

	async confirmEntry(entryId: string): Promise<void> {
		this.confirmedEntryIds.push(entryId);

		for (const entry of this.entries.values()) {
			if (entry.id === entryId) {
				entry.state = "confirmed";
			}
		}

		for (const token of this.confirmTokens.values()) {
			if (token.entryId === entryId) {
				token.entryState = "confirmed";
			}
		}
	}

	async consumeToken(tokenId: string, consumedAt: Date): Promise<void> {
		this.consumedTokenIds.push(tokenId);

		for (const token of this.confirmTokens.values()) {
			if (token.tokenId === tokenId) {
				token.consumedAt = consumedAt;
			}
		}
	}
}

class FakeMailer implements WaitlistMailer {
	public sent: Array<{
		email: string;
		origin: string;
		confirmToken: string;
		unsubscribeToken: string;
	}> = [];

	constructor(private readonly shouldFail = false) {}

	async sendConfirmation(input: {
		email: string;
		origin: string;
		confirmToken: string;
		unsubscribeToken: string;
	}): Promise<void> {
		if (this.shouldFail) {
			throw new Error("mail failed");
		}

		this.sent.push(input);
	}
}

describe("waitlist crypto", () => {
	it("creates url-safe non-guessable tokens and hashes them", async () => {
		const token = createOpaqueToken();
		expect(token).toMatch(/^[A-Za-z0-9_-]{43}$/);

		const digest = await hashOpaqueToken(token);
		expect(digest).toMatch(/^[a-f0-9]{64}$/);
	});
});

describe("waitlist submit flow", () => {
	it("accepts a fresh email and sends confirmation mail", async () => {
		const store = new FakeStore();
		const mailer = new FakeMailer();

		const result = await submitWaitlist(
			{
				email: "User@Example.com ",
				origin: "https://waddle.social",
			},
			{ store, mailer },
		);

		expect(result.kind).toBe("accepted");
		expect(mailer.sent).toHaveLength(1);
		expect(mailer.sent[0]?.email).toBe("user@example.com");
		expect(mailer.sent[0]?.confirmToken).toMatch(/^[A-Za-z0-9_-]{43}$/);
		expect(mailer.sent[0]?.unsubscribeToken).toMatch(/^[A-Za-z0-9_-]{43}$/);
	});

	it("returns a generic success result for duplicates without resending mail", async () => {
		const store = new FakeStore();
		const mailer = new FakeMailer();

		await submitWaitlist(
			{
				email: "user@example.com",
				origin: "https://waddle.social",
			},
			{ store, mailer },
		);

		const second = await submitWaitlist(
			{
				email: "user@example.com",
				origin: "https://waddle.social",
			},
			{ store, mailer },
		);

		expect(second.kind).toBe("accepted");
		expect(mailer.sent).toHaveLength(1);
	});

	it("rejects invalid email input without touching the store", async () => {
		const store = new FakeStore();
		const mailer = new FakeMailer();

		const result = await submitWaitlist(
			{
				email: "not-an-email",
				origin: "https://waddle.social",
			},
			{ store, mailer },
		);

		expect(result.kind).toBe("invalid");
		expect(mailer.sent).toHaveLength(0);
	});

	it("rolls back a new reservation if email sending fails", async () => {
		const store = new FakeStore();
		const mailer = new FakeMailer(true);

		const result = await submitWaitlist(
			{
				email: "user@example.com",
				origin: "https://waddle.social",
			},
			{ store, mailer },
		);

		expect(result.kind).toBe("retryable_error");
		expect(store.deletedEntryIds).toHaveLength(1);
	});
});

describe("waitlist action flows", () => {
	it("confirms a valid token and marks it consumed", async () => {
		const store = new FakeStore();
		const mailer = new FakeMailer();

		await submitWaitlist(
			{
				email: "user@example.com",
				origin: "https://waddle.social",
			},
			{ store, mailer },
		);

		const token = mailer.sent[0]?.confirmToken;
		expect(await inspectConfirmToken(token ?? null, store)).toBe("ready");
		expect(await confirmWaitlist(token ?? null, store)).toBe("confirmed");
		expect(await inspectConfirmToken(token ?? null, store)).toBe("handled");
		expect(store.confirmedEntryIds).toHaveLength(1);
		expect(store.consumedTokenIds).toHaveLength(1);
	});

	it("treats unknown confirmation links as invalid", async () => {
		const store = new FakeStore();
		expect(await inspectConfirmToken("missing", store)).toBe("invalid");
		expect(await confirmWaitlist("missing", store)).toBe("invalid");
	});

	it("removes the entry on unsubscribe", async () => {
		const store = new FakeStore();
		const mailer = new FakeMailer();

		await submitWaitlist(
			{
				email: "user@example.com",
				origin: "https://waddle.social",
			},
			{ store, mailer },
		);

		const token = mailer.sent[0]?.unsubscribeToken;
		expect(await inspectUnsubscribeToken(token ?? null, store)).toBe("ready");
		expect(await unsubscribeWaitlist(token ?? null, store)).toBe("removed");
		expect(await inspectUnsubscribeToken(token ?? null, store)).toBe("invalid");
		expect(store.deletedEntryIds).toHaveLength(1);
	});
});
