import type { APIRoute } from "astro";

import {
	WAITLIST_INVALID_MESSAGE,
	WAITLIST_QUERY_KEY,
	WAITLIST_RETRY_MESSAGE,
	WAITLIST_SUCCESS_MESSAGE,
	WAITLIST_TURNSTILE_ACTION,
	WAITLIST_UNAVAILABLE_MESSAGE,
	WAITLIST_VERIFICATION_MESSAGE,
} from "../../lib/waitlist/constants";
import { isValidEmail, normalizeEmail } from "../../lib/waitlist/crypto";
import {
	getWaitlistRuntime,
	getWaitlistTurnstileSecretKey,
} from "../../lib/waitlist/runtime";
import { submitWaitlist } from "../../lib/waitlist/service";
import { verifyTurnstileToken } from "../../lib/waitlist/turnstile";

function wantsJson(request: Request): boolean {
	const accept = request.headers.get("accept") ?? "";
	return accept.includes("application/json");
}

function redirectHome(request: Request, state: string): Response {
	const url = new URL("/", request.url);
	url.searchParams.set(WAITLIST_QUERY_KEY, state);
	return Response.redirect(url.toString(), 303);
}

export const POST: APIRoute = async ({ request }) => {
	const formData = await request.formData();
	const email = formData.get("email");
	const turnstileToken = formData.get("cf-turnstile-response");
	const emailValue = typeof email === "string" ? email : "";
	const normalizedEmail = normalizeEmail(emailValue);

	if (!isValidEmail(normalizedEmail)) {
		if (!wantsJson(request)) {
			return redirectHome(request, "invalid");
		}

		return Response.json(
			{
				ok: false,
				status: "invalid",
				message: WAITLIST_INVALID_MESSAGE,
			},
			{ status: 400 },
		);
	}

	const turnstileSecretKey = await getWaitlistTurnstileSecretKey();
	if (!turnstileSecretKey) {
		if (!wantsJson(request)) {
			return redirectHome(request, "offline");
		}

		return Response.json(
			{
				ok: false,
				status: "unavailable",
				message: WAITLIST_UNAVAILABLE_MESSAGE,
			},
			{ status: 503 },
		);
	}

	const verification = await verifyTurnstileToken({
		token: typeof turnstileToken === "string" ? turnstileToken : null,
		secretKey: turnstileSecretKey,
		remoteIp: request.headers.get("CF-Connecting-IP"),
		expectedAction: WAITLIST_TURNSTILE_ACTION,
	});

	if (verification.kind === "failed") {
		if (!wantsJson(request)) {
			return redirectHome(request, "verify");
		}

		return Response.json(
			{
				ok: false,
				status: "verification_failed",
				message: WAITLIST_VERIFICATION_MESSAGE,
			},
			{ status: 400 },
		);
	}

	const { store, mailer } = getWaitlistRuntime();
	const result = await submitWaitlist(
		{
			email: normalizedEmail,
			origin: new URL(request.url).origin,
		},
		{ store, mailer },
	);

	if (!wantsJson(request)) {
		if (result.kind === "accepted") {
			return redirectHome(request, "submitted");
		}

		if (result.kind === "invalid") {
			return redirectHome(request, "invalid");
		}

		return redirectHome(request, "retry");
	}

	const payload = {
		ok: result.kind === "accepted",
		status: result.kind,
		message: result.kind === "accepted" ? WAITLIST_SUCCESS_MESSAGE : WAITLIST_RETRY_MESSAGE,
	};

	const status = result.kind === "accepted" ? 200 : 503;

	return Response.json(payload, { status });
};
