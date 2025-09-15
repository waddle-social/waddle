import type { APIRoute } from "astro";
import { createAuth } from "../../../lib/auth/better-auth";

export const POST: APIRoute = async (context) => {
  const { request, locals } = context;
  const auth = await createAuth(locals.runtime.env.DB, locals.runtime.env, request);

  try {
    // Clear the session
    await auth.api.signOut({
      headers: request.headers,
    });

    // Redirect to homepage
    return new Response(null, {
      status: 302,
      headers: {
        Location: "/",
        "Set-Cookie": "session=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0",
      },
    });
  } catch (error) {
    console.error("Logout error:", error);
    // Even if there's an error, clear the cookie and redirect
    return new Response(null, {
      status: 302,
      headers: {
        Location: "/",
        "Set-Cookie": "session=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0",
      },
    });
  }
};
