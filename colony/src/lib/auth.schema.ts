import { betterAuth } from "better-auth";
import { jwt, oidcProvider } from "better-auth/plugins";

import { database } from "./auth-database";

export const auth = betterAuth({
  baseURL: process.env.BETTER_AUTH_URL,
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
