import { betterAuth } from "better-auth";
import { jwt, oidcProvider } from "better-auth/plugins";
import { env } from "cloudflare:workers";

import { database } from "./auth-database";
function createAuth(secret: string, githubClientSecret: string) {
  return betterAuth({
    baseURL: env.BETTER_AUTH_URL,
    secret,
    database,
    trustedOrigins: [new URL(env.BETTER_AUTH_URL).origin],
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
        getAdditionalUserInfoClaim: (user, _scopes, client) => {
          if (client.metadata?.product !== "server") {
            return {};
          }

          const picture = typeof user.image === "string" ? user.image.trim() : "";
          const githubUsername =
            typeof user.githubUsername === "string"
              ? user.githubUsername.trim()
              : "";
          const claims: Record<string, string> = {};

          if (githubUsername) {
            claims.preferred_username = githubUsername;
          }
          if (picture) {
            claims.picture = picture;
          }

          return claims;
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
  ]).then(([secret, githubClientSecret]) => createAuth(secret, githubClientSecret));

  return authPromise;
}
