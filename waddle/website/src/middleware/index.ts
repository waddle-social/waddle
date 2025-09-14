import { WORKOS_COOKIE_SECRET } from "astro:env/server";
import { defineMiddleware, sequence } from "astro:middleware";
import { SESSION_COOKIE_NAME, workos } from "@/lib/workos";

// Only these paths require authentication
// All other paths (including "/") will pass through without auth checks
// Add new protected paths here as needed
const protectedPaths = ["/chat"];

const authMiddleware = defineMiddleware(async (context, next) => {
	const url = new URL(context.request.url);
	const { cookies, redirect } = context;

	// Check if the current path or any parent path requires authentication
	const requiresAuth = protectedPaths.some(
		(path) => url.pathname === path || url.pathname.startsWith(`${path}/`),
	);

	const cookie = cookies.get(SESSION_COOKIE_NAME);

	if (!cookie) {
		// If protected path and no cookie, redirect to login
		if (requiresAuth) {
			return redirect("/api/auth/login");
		}
		// Otherwise, continue without user data
		return next();
	}

	// Try to load and authenticate the session
	const session = workos.userManagement.loadSealedSession({
		sessionData: cookie.value,
		cookiePassword: WORKOS_COOKIE_SECRET,
	});

	const result = await session.authenticate();

	if (result.authenticated) {
		context.locals.user = result.user;
		return next();
	}

	// If not authenticated and it's a protected path
	if (!result.authenticated && requiresAuth) {
		if (result.reason === "no_session_cookie_provided") {
			return redirect("/api/auth/login");
		}

		// Try to refresh the session
		try {
			const refreshResult = await session.refresh();

			if (!refreshResult.authenticated) {
				return redirect("/api/auth/login");
			}

			context.locals.user = refreshResult.user;

			cookies.set(SESSION_COOKIE_NAME, refreshResult.sealedSession as string, {
				path: "/",
				httpOnly: true,
				secure: import.meta.env.MODE === "production",
				sameSite: "lax",
			});

			return redirect(url.pathname);
		} catch (_error) {
			cookies.delete(SESSION_COOKIE_NAME);
			return redirect("/api/auth/login");
		}
	}

	// For non-protected paths, just continue even if not authenticated
	return next();
});

export const onRequest = sequence(authMiddleware);
