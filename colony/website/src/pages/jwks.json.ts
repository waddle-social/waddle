import type { APIRoute } from "astro";

export const ALL: APIRoute = async () =>
	new Response(
		JSON.stringify({
			error: "gone",
			message: "Legacy JWKS route removed.",
		}),
		{ status: 410, headers: { "Content-Type": "application/json" } }
	);
