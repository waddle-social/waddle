import type { APIRoute } from "astro";

import { getWaitlistRuntime } from "../../../lib/waitlist/runtime";
import { unsubscribeWaitlist } from "../../../lib/waitlist/service";

function redirectToResult(request: Request, result: string): Response {
	const url = new URL("/waitlist/unsubscribe", request.url);
	url.searchParams.set("result", result);
	return Response.redirect(url.toString(), 303);
}

export const POST: APIRoute = async ({ request }) => {
	const formData = await request.formData();
	const token = formData.get("token");
	const { store } = getWaitlistRuntime();

	const result = await unsubscribeWaitlist(
		typeof token === "string" ? token : null,
		store,
	);
	return redirectToResult(request, result === "removed" ? "removed" : "stale");
};
