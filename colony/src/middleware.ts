import { defineMiddleware } from "astro:middleware";

import { auth } from "./lib/auth";

export const onRequest = defineMiddleware(async (context, next) => {
  const session = await auth.api.getSession({
    headers: context.request.headers,
  });

  context.locals.user = session?.user ?? null;
  context.locals.session = session?.session ?? null;

  return next();
});
