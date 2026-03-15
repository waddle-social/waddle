import { drizzleAdapter } from "@better-auth/drizzle-adapter";
import { betterAuth } from "better-auth";
import { env } from "cloudflare:workers";

import { db } from "../db";
import { account, session, user, verification } from "../db/schema";

const database = drizzleAdapter(db, {
  provider: "sqlite",
  schema: {
    user,
    session,
    account,
    verification,
  },
}) as unknown as Parameters<typeof betterAuth>[0]["database"];

function createAuth(secret: string, githubClientSecret: string) {
  return betterAuth({
    baseURL: env.BETTER_AUTH_URL,
    secret,
    database,
    socialProviders: {
      github: {
        clientId: env.GITHUB_CLIENT_ID,
        clientSecret: githubClientSecret,
        scope: ["read:user", "user:email"],
      },
    },
  });
}

let authPromise: Promise<ReturnType<typeof createAuth>> | undefined;

export function getAuth() {
  authPromise ??= Promise.all([
    env.BETTER_AUTH_SECRET.get(),
    env.GITHUB_CLIENT_SECRET.get(),
  ]).then(([secret, githubClientSecret]) => createAuth(secret, githubClientSecret));

  return authPromise;
}
