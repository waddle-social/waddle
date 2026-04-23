import { env } from "cloudflare:workers";

import type { D1Database } from "@cloudflare/workers-types";

export type EmailAddress = {
	email: string;
	name: string;
};

export type SendEmailBinding = {
	send(message: {
		from: string | EmailAddress;
		to: string | string[];
		subject: string;
		replyTo?: string | EmailAddress;
		cc?: string | string[];
		bcc?: string | string[];
		headers?: Record<string, string>;
		text?: string;
		html?: string;
		attachments?: unknown[];
	}): Promise<unknown>;
};

export type WebsiteWorkerEnv = {
	DB: D1Database;
	EMAIL: SendEmailBinding;
};

export function getWorkerEnv(): WebsiteWorkerEnv {
	return env as WebsiteWorkerEnv;
}
