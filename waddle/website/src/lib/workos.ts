import {
	WORKOS_API_KEY,
	WORKOS_CLIENT_ID,
	WORKOS_COOKIE_SECRET,
} from "astro:env/server";
import { WorkOS } from "@workos-inc/node";
import type { AstroCookies } from "astro";

export const SESSION_COOKIE_NAME = "workos-session";

export const workos = new WorkOS(WORKOS_API_KEY, {
	clientId: WORKOS_CLIENT_ID,
});

const getSession = async (cookies: AstroCookies) => {
	const cookieSession = cookies.get(SESSION_COOKIE_NAME);

	const session = workos.userManagement.loadSealedSession({
		sessionData: cookieSession?.value as string,
		cookiePassword: WORKOS_COOKIE_SECRET,
	});

	return await session.authenticate();
};

export const isAuthenticated = async (cookies: AstroCookies) => {
	const result = await getSession(cookies);
	return result.authenticated;
};

export const getUser = async (cookies: AstroCookies) => {
	const result = await getSession(cookies);
	if (result.authenticated) {
		return result.user;
	}
	return null;
};
