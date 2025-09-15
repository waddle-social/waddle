import type { APIRoute } from "astro";
import { createAuth } from "../../../lib/auth/better-auth";

export const ALL: APIRoute = async (context) => {
	const { request, locals } = context;
	const auth = await createAuth(locals.runtime.env.DB, locals.runtime.env, request);
	return auth.handler(request);
};
