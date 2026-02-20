import type { APIRoute } from "astro";

const message = {
	error: "gone",
	message: "Auth disabled in colony website. Use server /api/auth endpoints.",
};

export const ALL: APIRoute = async () =>
	new Response(JSON.stringify(message), {
		status: 410,
		headers: { "Content-Type": "application/json" },
	});
