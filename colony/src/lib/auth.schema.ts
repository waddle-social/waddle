import { betterAuth } from "better-auth";
import { oauthProvider } from "@better-auth/oauth-provider";
import { jwt } from "better-auth/plugins";

import { database } from "./auth-database";
import {
  getServerIdTokenClaims,
  getServerUserInfoClaims,
  oauthClaimsSupported,
} from "./oauth-claims";

export const auth = betterAuth({
  baseURL: process.env.BETTER_AUTH_URL,
  disabledPaths: ["/token"],
  secret: process.env.BETTER_AUTH_SECRET,
  database,
  trustedOrigins: [
    process.env.BETTER_AUTH_URL
      ? new URL(process.env.BETTER_AUTH_URL).origin
      : "http://localhost:4321",
  ],
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
      clientId: process.env.GITHUB_CLIENT_ID,
      clientSecret: process.env.GITHUB_CLIENT_SECRET,
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
        issuer: process.env.BETTER_AUTH_URL ?? "http://localhost:4321",
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
