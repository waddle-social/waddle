import type { APIRoute } from "astro";

import {
	WAITLIST_INVALID_MESSAGE,
	WAITLIST_QUERY_KEY,
	WAITLIST_RETRY_MESSAGE,
	WAITLIST_SUCCESS_MESSAGE,
} from "../../lib/waitlist/constants";
import { getWaitlistRuntime } from "../../lib/waitlist/runtime";
import { submitWaitlist } from "../../lib/waitlist/service";

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

	const { store, mailer } = getWaitlistRuntime();
	const result = await submitWaitlist(
		{
			email: typeof email === "string" ? email : "",
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
		message:
			result.kind === "accepted"
				? WAITLIST_SUCCESS_MESSAGE
				: result.kind === "invalid"
					? WAITLIST_INVALID_MESSAGE
					: WAITLIST_RETRY_MESSAGE,
	};

	const status =
		result.kind === "accepted" ? 200 : result.kind === "invalid" ? 400 : 503;

	return Response.json(payload, { status });
};
