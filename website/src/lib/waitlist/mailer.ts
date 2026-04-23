import { buildWaitlistEmail } from "./email";
import { WAITLIST_SENDER } from "./constants";

import type { SendEmailBinding } from "../cloudflare/env";

export type WaitlistMailer = {
	sendConfirmation(input: {
		email: string;
		origin: string;
		confirmToken: string;
		unsubscribeToken: string;
	}): Promise<void>;
};

export function createWaitlistMailer(binding: SendEmailBinding): WaitlistMailer {
	return {
		async sendConfirmation(input) {
			const confirmUrl = new URL("/waitlist/confirm", input.origin);
			confirmUrl.searchParams.set("token", input.confirmToken);

			const unsubscribeUrl = new URL("/waitlist/unsubscribe", input.origin);
			unsubscribeUrl.searchParams.set("token", input.unsubscribeToken);

			const { subject, html, text } = buildWaitlistEmail({
				logoUrl: new URL("/logo.png", input.origin).toString(),
				confirmUrl: confirmUrl.toString(),
				unsubscribeUrl: unsubscribeUrl.toString(),
			});

			await binding.send({
				from: WAITLIST_SENDER,
				to: input.email,
				subject,
				html,
				text,
			});
		},
	};
}
