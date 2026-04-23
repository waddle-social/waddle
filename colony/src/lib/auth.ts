import { betterAuth } from "better-auth";
import { oauthProvider } from "@better-auth/oauth-provider";
import { jwt } from "better-auth/plugins";
import { env } from "cloudflare:workers";

import { database } from "./auth-database";
import {
  getServerIdTokenClaims,
  getServerUserInfoClaims,
  oauthClaimsSupported,
} from "./oauth-claims";
function createAuth(secret: string, githubClientSecret: string) {
  return betterAuth({
    baseURL: env.BETTER_AUTH_URL,
    disabledPaths: ["/token"],
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
        jwt: {
          issuer: env.BETTER_AUTH_URL,
        },
      }),
      oauthProvider({
        loginPage: "/sign-in",
        consentPage: "/oauth/consent",
        allowDynamicClientRegistration: true,
        allowUnauthenticatedClientRegistration: true,
        customIdTokenClaims: getServerIdTokenClaims,
        customUserInfoClaims: getServerUserInfoClaims,
        advertisedMetadata: {
          claims_supported: [...oauthClaimsSupported],
        },
        silenceWarnings: {
          oauthAuthServerConfig: true,
          openidConfig: true,
        },
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
