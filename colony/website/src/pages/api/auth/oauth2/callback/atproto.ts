import type { APIRoute } from "astro";

const message = {
	error: "gone",
	message: "Legacy auth route removed. Use server /v2/auth endpoints.",
};

export const ALL: APIRoute = async () =>
	new Response(JSON.stringify(message), {
		status: 410,
		headers: { "Content-Type": "application/json" },
	});
