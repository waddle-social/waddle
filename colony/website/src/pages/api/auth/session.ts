import type { APIRoute } from "astro";
import { createAuth } from "../../../lib/auth/better-auth";
import { getAllowedOrigins } from "../../../lib/auth/redirect";

export const GET: APIRoute = async (context) => {
  const { request, locals } = context;
  const auth = await createAuth(locals.runtime.env.DB, locals.runtime.env, request);
  const colonyOrigin = new URL(request.url).origin;
  const allowedOrigins = new Set(getAllowedOrigins(colonyOrigin, locals.runtime.env ?? {}));
  const requestOrigin = request.headers.get("Origin");
  const allowOrigin = requestOrigin && allowedOrigins.has(requestOrigin) ? requestOrigin : null;

  const createJsonResponse = (payload: unknown, status = 200) => {
    const headers = new Headers({ "Content-Type": "application/json" });
    if (allowOrigin) {
      headers.set("Access-Control-Allow-Origin", allowOrigin);
      headers.set("Access-Control-Allow-Credentials", "true");
      headers.append("Vary", "Origin");
    }
    return new Response(JSON.stringify(payload), { status, headers });
  };

  try {
    const session = await auth.api.getSession({
      headers: request.headers,
    });

    if (session) {
      return createJsonResponse({
        authenticated: true,
        user: session.user,
        session: session.session,
      });
    }

    return createJsonResponse({
      authenticated: false,
    });
  } catch (error) {
    console.error("Session check error:", error);
    return createJsonResponse({
      authenticated: false,
      error: "Failed to check session",
    });
  }
};
