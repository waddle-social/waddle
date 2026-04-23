import { WAITLIST_INVALID_MESSAGE, WAITLIST_RETRY_MESSAGE, WAITLIST_SUCCESS_MESSAGE } from "./constants";
import { createOpaqueToken, hashOpaqueToken, isValidEmail, normalizeEmail } from "./crypto";

import type { WaitlistMailer } from "./mailer";
import type { ConfirmTokenLookup, WaitlistStore } from "./repository";

export type SubmitWaitlistResult =
	| { kind: "accepted"; message: string }
	| { kind: "invalid"; message: string }
	| { kind: "retryable_error"; message: string };

export type ConfirmTokenStatus = "ready" | "handled" | "invalid";
export type UnsubscribeTokenStatus = "ready" | "invalid";

async function lookupConfirmToken(
	token: string,
	store: WaitlistStore,
): Promise<ConfirmTokenLookup | null> {
	return store.findConfirmToken(await hashOpaqueToken(token));
}

export async function submitWaitlist(
	input: {
		email: string;
		origin: string;
	},
	deps: {
		store: WaitlistStore;
		mailer: WaitlistMailer;
	},
): Promise<SubmitWaitlistResult> {
	const email = normalizeEmail(input.email);
	if (!isValidEmail(email)) {
		return { kind: "invalid", message: WAITLIST_INVALID_MESSAGE };
	}

	const confirmToken = createOpaqueToken();
	const unsubscribeToken = createOpaqueToken();

	const reservation = await deps.store.reserveEntry({
		email,
		confirmTokenHash: await hashOpaqueToken(confirmToken),
		unsubscribeTokenHash: await hashOpaqueToken(unsubscribeToken),
	});

	if (reservation.kind === "duplicate") {
		return { kind: "accepted", message: WAITLIST_SUCCESS_MESSAGE };
	}

	try {
		await deps.mailer.sendConfirmation({
			email,
			origin: input.origin,
			confirmToken,
			unsubscribeToken,
		});
		return { kind: "accepted", message: WAITLIST_SUCCESS_MESSAGE };
	} catch {
		await deps.store.deleteEntry(reservation.entryId);
		return { kind: "retryable_error", message: WAITLIST_RETRY_MESSAGE };
	}
}

export async function inspectConfirmToken(
	token: string | null,
	store: WaitlistStore,
): Promise<ConfirmTokenStatus> {
	if (!token) {
		return "invalid";
	}

	const match = await lookupConfirmToken(token, store);
	if (!match) {
		return "invalid";
	}

	if (match.consumedAt || match.entryState === "confirmed") {
		return "handled";
	}

	return "ready";
}

export async function confirmWaitlist(
	token: string | null,
	store: WaitlistStore,
): Promise<"confirmed" | "handled" | "invalid"> {
	if (!token) {
		return "invalid";
	}

	const match = await lookupConfirmToken(token, store);
	if (!match) {
		return "invalid";
	}

	if (match.consumedAt || match.entryState === "confirmed") {
		return "handled";
	}

	const now = new Date();
	await store.confirmEntry(match.entryId, now);
	await store.consumeToken(match.tokenId, now);
	return "confirmed";
}

export async function inspectUnsubscribeToken(
	token: string | null,
	store: WaitlistStore,
): Promise<UnsubscribeTokenStatus> {
	if (!token) {
		return "invalid";
	}

	const match = await store.findUnsubscribeToken(await hashOpaqueToken(token));
	return match ? "ready" : "invalid";
}

export async function unsubscribeWaitlist(
	token: string | null,
	store: WaitlistStore,
): Promise<"removed" | "invalid"> {
	if (!token) {
		return "invalid";
	}

	const match = await store.findUnsubscribeToken(await hashOpaqueToken(token));
	if (!match) {
		return "invalid";
	}

	await store.deleteEntry(match.entryId);
	return "removed";
}
