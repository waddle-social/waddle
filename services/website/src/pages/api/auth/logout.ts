import { SESSION_COOKIE_NAME, workos } from "@/lib/workos";
import type { APIRoute } from "astro";
import { WORKOS_COOKIE_SECRET } from "astro:env/server";

export const GET: APIRoute = async ({
    cookies,
    site,
    redirect,
}): Promise<Response> => {
    const session = workos.userManagement.loadSealedSession({
        sessionData: cookies.get(SESSION_COOKIE_NAME)?.value as string,
        cookiePassword: WORKOS_COOKIE_SECRET,
    });

    const logoutUrl = await session.getLogoutUrl({
        returnTo: new URL("/", site).href
    });

    cookies.delete(SESSION_COOKIE_NAME, {
        path: "/",
        secure: import.meta.env.MODE === "production",
        httpOnly: true,
        sameSite: "lax",
    });

    console.log(`Logout succeeded. Redirecting to '${logoutUrl}'`)

    return redirect(logoutUrl);
}
