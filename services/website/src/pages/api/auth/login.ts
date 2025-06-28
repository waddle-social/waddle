import { workos } from "@/lib/workos";
import type { APIRoute } from "astro";
import { WORKOS_CLIENT_ID } from "astro:env/server";

export const GET: APIRoute = async ({
    site,
    redirect,
}): Promise<Response> => {
    const authorizationUrl = workos.userManagement.getAuthorizationUrl({
        provider: "authkit",
        redirectUri: new URL("/api/auth/callback", site).href,
        clientId: WORKOS_CLIENT_ID,
    });

    return redirect(authorizationUrl);
}
