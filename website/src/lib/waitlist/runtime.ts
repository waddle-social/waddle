import { getWaitlistDb } from "../cloudflare/db";
import { getWorkerEnv } from "../cloudflare/env";
import { createWaitlistMailer } from "./mailer";
import { createDrizzleWaitlistStore } from "./repository";

export function getWaitlistTurnstileSiteKey() {
	const workerEnv = getWorkerEnv();

	return workerEnv.TURNSTILE_SITE_KEY?.trim() ?? "";
}

export async function getWaitlistTurnstileSecretKey() {
	const workerEnv = getWorkerEnv();
	const secret = await workerEnv.TURNSTILE_SECRET_KEY?.get();

	return secret?.trim() ?? "";
}

export function getWaitlistRuntime() {
	const workerEnv = getWorkerEnv();
	const db = getWaitlistDb(workerEnv.DB);

	return {
		store: createDrizzleWaitlistStore(db),
		mailer: createWaitlistMailer(workerEnv.EMAIL),
	};
}
