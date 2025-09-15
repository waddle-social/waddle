import type { APIRoute } from "astro";
import { createAuth } from "../../../lib/auth/better-auth";

export const POST: APIRoute = async ({ request, locals }) => {
	try {
		// Get environment variables
		const env = locals.runtime.env;
		const userCacheKV = env.USER_CACHE;

		// Create auth instance
		const auth = await createAuth(env.DB, env, request);
		const context = await auth.$context;

		// Verify session using Better Auth API (handles cookie name/signature)
		const sessionResult = await auth.api.getSession({ headers: request.headers });
		if (!sessionResult) {
			return new Response(JSON.stringify({ error: "Not authenticated" }), {
				status: 401,
				headers: { "Content-Type": "application/json" },
			});
		}

		// Get the request body
		const body = await request.json();
		const { email } = body;

		if (!email || !email.includes("@")) {
			return new Response(JSON.stringify({ error: "Invalid email address" }), {
				status: 400,
				headers: { "Content-Type": "application/json" }
			});
		}

		// Get the user
		const user = await context.internalAdapter.findUserById(sessionResult.session.userId);
		if (!user) {
			return new Response(JSON.stringify({ error: "User not found" }), {
				status: 404,
				headers: { "Content-Type": "application/json" }
			});
		}

		// Check if email is already taken
		const existingUser = await context.internalAdapter.findUserByEmail(email);
		if (existingUser && existingUser.id !== user.id) {
			return new Response(JSON.stringify({ error: "Email already in use" }), {
				status: 400,
				headers: { "Content-Type": "application/json" }
			});
		}

		// Update the user's email
		await context.internalAdapter.updateUser(user.id, {
			email,
			emailVerified: false // Require verification for new email
		});

		// Get user's DID from cookie
		const sessionCookie = request.headers.get("cookie") || "";
		const didCookie = sessionCookie.split(";").find(c => c.trim().startsWith("atproto_user_did="));
		if (didCookie && userCacheKV) {
			const userDid = decodeURIComponent(didCookie.split("=")[1]);

			// Update KV cache
			try {
				const cachedData = await userCacheKV.get(`user:${userDid}`);
				if (cachedData) {
					const parsed = JSON.parse(cachedData);
					parsed.email = email;
					parsed.updatedAt = new Date().toISOString();

					await userCacheKV.put(
						`user:${userDid}`,
						JSON.stringify(parsed),
						{ expirationTtl: 60 * 60 * 24 * 30 } // 30 days
					);
				}
			} catch (e) {
				console.error("Failed to update KV cache:", e);
			}
		}

		return new Response(JSON.stringify({
			success: true,
			message: "Email updated successfully. Please verify your new email address."
		}), {
			status: 200,
			headers: { "Content-Type": "application/json" }
		});

	} catch (error) {
		console.error("Update email error:", error);
		return new Response(JSON.stringify({ error: "Server error" }), {
			status: 500,
			headers: { "Content-Type": "application/json" }
		});
	}
};
