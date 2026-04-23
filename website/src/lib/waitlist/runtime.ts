import { getWaitlistDb } from "../cloudflare/db";
import { getWorkerEnv } from "../cloudflare/env";
import { createWaitlistMailer } from "./mailer";
import { createDrizzleWaitlistStore } from "./repository";

export function getWaitlistRuntime() {
	const workerEnv = getWorkerEnv();
	const db = getWaitlistDb(workerEnv.DB);

	return {
		store: createDrizzleWaitlistStore(db),
		mailer: createWaitlistMailer(workerEnv.EMAIL),
	};
}
