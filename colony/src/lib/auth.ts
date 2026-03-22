import { betterAuth } from "better-auth";
import { jwt, oidcProvider } from "better-auth/plugins";
import { env } from "cloudflare:workers";

import { database } from "./auth-database";
import { buildTrustedOidcClients, trustedOidcOrigins } from "./oidc";

function createAuth(
  secret: string,
  githubClientSecret: string,
  serverOidcClientSecret: string,
) {
  return betterAuth({
    baseURL: env.BETTER_AUTH_URL,
    secret,
    database,
    trustedOrigins: trustedOidcOrigins,
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
        loginPage: "/sign-in",
        consentPage: "/oauth/consent",
        allowDynamicClientRegistration: true,
        trustedClients: buildTrustedOidcClients(serverOidcClientSecret),
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
    env.WADDLE_SERVER_OIDC_CLIENT_SECRET.get(),
  ]).then(([secret, githubClientSecret, serverOidcClientSecret]) =>
    createAuth(secret, githubClientSecret, serverOidcClientSecret),
  );

  return authPromise;
}
