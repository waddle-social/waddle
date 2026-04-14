import { betterAuth } from "better-auth";
import { jwt, oidcProvider } from "better-auth/plugins";
import { env } from "cloudflare:workers";

import { database } from "./auth-database";
import {
  buildTrustedOidcClients,
  SERVER_OIDC_CLIENT_ID,
  trustedOidcOrigins,
} from "./oidc";

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
    user: {
      additionalFields: {
        githubUsername: {
          type: "string",
          required: true,
        },
      },
    },
    socialProviders: {
      github: {
        clientId: env.GITHUB_CLIENT_ID,
        clientSecret: githubClientSecret,
        scope: ["read:user", "user:email"],
        mapProfileToUser: (profile) => ({
          githubUsername: profile.login,
        }),
        overrideUserInfoOnSignIn: true,
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
        getAdditionalUserInfoClaim: (user, _scopes, client) => {
          if (client.clientId !== SERVER_OIDC_CLIENT_ID) {
            return {};
          }

          const githubUsername =
            typeof user.githubUsername === "string"
              ? user.githubUsername.trim()
              : "";
          if (!githubUsername) {
            return {};
          }

          return { preferred_username: githubUsername };
        },
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
