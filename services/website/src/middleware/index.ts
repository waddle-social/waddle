import { SESSION_COOKIE_NAME, workos } from '@/lib/workos';
import { WORKOS_COOKIE_SECRET } from "astro:env/server";
import { defineMiddleware, sequence } from 'astro:middleware';

const bypassUrls = ["/api/auth/login", "/api/auth/logout"];

const authMiddleware = defineMiddleware(async (context, next) => {
    console.log(context.url.pathname)

    const url = new URL(context.request.url);

    if (bypassUrls.includes(url.pathname)) {
        return next();
    }

    const { cookies, redirect } = context;

    const cookie = cookies.get(SESSION_COOKIE_NAME);

    if (!cookie) {
        console.log("no cookie")
        return next()
    }

    const session = workos.userManagement.loadSealedSession({
        sessionData: cookie.value,
        cookiePassword: WORKOS_COOKIE_SECRET,
    });

    const result = await session.authenticate();

    if (result.authenticated) {
        context.locals.user = result.user;

        console.log("already authenticated")
        return next();
    }

    if (!result.authenticated && result.reason === 'no_session_cookie_provided') {
        console.log("no session cookie provided")
        return next();
    }

    try {
        const result = await session.refresh();

        if (!result.authenticated) {
            console.log("no session refresh")
            return next();
        }

        context.locals.user = result.user;

        cookies.set(SESSION_COOKIE_NAME, result.sealedSession as string, {
            path: '/',
            httpOnly: true,
            secure: import.meta.env.MODE === "production",
            sameSite: 'lax',
        });

        console.log(context.locals.user)

        return redirect(url.pathname)
    } catch (error) {
        console.log(error)
        cookies.delete(SESSION_COOKIE_NAME);
        return redirect('/api/auth/login');
    }
})

export const onRequest = sequence(authMiddleware);
