import type { APIRoute } from "astro";
import { createAuth } from "../../../lib/auth/better-auth";

export const GET: APIRoute = async (context) => {
  const { request, locals } = context;
  const auth = await createAuth(locals.runtime.env.DB, locals.runtime.env, request);

  try {
    const session = await auth.api.getSession({
      headers: request.headers,
    });

    if (session) {
      return new Response(
        JSON.stringify({
          authenticated: true,
          user: session.user,
          session: session.session,
        }),
        {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }
      );
    }

    return new Response(
      JSON.stringify({
        authenticated: false,
      }),
      {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }
    );
  } catch (error) {
    console.error("Session check error:", error);
    return new Response(
      JSON.stringify({
        authenticated: false,
        error: "Failed to check session",
      }),
      {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }
    );
  }
};
