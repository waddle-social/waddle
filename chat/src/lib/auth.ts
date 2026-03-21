import { drizzleAdapter } from "@better-auth/drizzle-adapter";
import { betterAuth } from "better-auth";
import { genericOAuth } from "better-auth/plugins";
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

function createAuth(secret: string) {
  return betterAuth({
    baseURL: env.BETTER_AUTH_URL,
    secret,
    database,
    plugins: [
      genericOAuth({
        config: [
          {
            providerId: "colony",
            discoveryUrl: `${env.COLONY_AUTH_URL}/api/auth/.well-known/openid-configuration`,
            issuer: env.COLONY_AUTH_URL,
            clientId: env.COLONY_OIDC_CLIENT_ID,
            scopes: ["openid", "profile", "email"],
            pkce: true,
            requireIssuerValidation: true,
            overrideUserInfo: true,
          },
        ],
      }),
    ],
  });
}

let authPromise: Promise<ReturnType<typeof createAuth>> | undefined;

export function getAuth() {
  authPromise ??= env.BETTER_AUTH_SECRET.get().then((secret) => createAuth(secret));

  return authPromise;
}
