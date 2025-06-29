import { WORKOS_CLIENT_ID, WORKOS_COOKIE_SECRET } from "astro:env/server";
import type { APIRoute } from "astro";
import { SESSION_COOKIE_NAME, workos } from "@/lib/workos";

export const GET: APIRoute = async ({
	cookies,
	request,
	redirect,
}): Promise<Response> => {
	const url = new URL(request.url);
	const searchParams = url.searchParams;

	const code = searchParams.get("code");

	if (!code) {
		return new Response(null, {
			status: 401,
			statusText: "Authentication failed",
		});
	}

	try {
		const { sealedSession } = await workos.userManagement.authenticateWithCode({
			clientId: WORKOS_CLIENT_ID,
			code,
			session: {
				sealSession: true,
				cookiePassword: WORKOS_COOKIE_SECRET,
			},
		});

		if (!sealedSession) {
			return redirect("/api/auth/login");
		}

		cookies.set(SESSION_COOKIE_NAME, sealedSession, {
			path: "/",
			httpOnly: true,
			secure: import.meta.env.MODE === "production",
			sameSite: "lax",
		});

		return redirect("/chat");
	} catch (error) {
		console.log(error);

		return redirect("/api/auth/login");
	}
};
