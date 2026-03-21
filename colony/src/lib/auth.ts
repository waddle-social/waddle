import { betterAuth } from "better-auth";
import { jwt, oidcProvider } from "better-auth/plugins";
import { env } from "cloudflare:workers";

import { database } from "./auth-database";
import { trustedOidcClients } from "./oidc";

function createAuth(secret: string, githubClientSecret: string) {
  return betterAuth({
    baseURL: env.BETTER_AUTH_URL,
    secret,
    database,
    disabledPaths: ["/token", "/oauth2/register"],
    trustedOrigins: ["https://chat.waddle.social", "http://localhost:4321"],
    socialProviders: {
      github: {
        clientId: env.GITHUB_CLIENT_ID,
        clientSecret: githubClientSecret,
        scope: ["read:user", "user:email"],
      },
    },
    plugins: [
      jwt({
        disableSettingJwtHeader: true,
      }),
      oidcProvider({
        loginPage: "/",
        trustedClients: trustedOidcClients,
        useJWTPlugin: true,
      }),
    ],
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
