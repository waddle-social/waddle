import { betterAuth } from "better-auth";
import { jwt, oidcProvider } from "better-auth/plugins";

import { database } from "./auth-database";
import {
  buildTrustedOidcClients,
  buildTrustedOidcOrigins,
  resolveServerOidcRedirectUrls,
  SERVER_OIDC_CLIENT_ID,
} from "./oidc";

const serverRedirectUrls = resolveServerOidcRedirectUrls(
  process.env.WADDLE_SERVER_OIDC_REDIRECT_URLS,
);

export const auth = betterAuth({
  baseURL: process.env.BETTER_AUTH_URL,
  secret: process.env.BETTER_AUTH_SECRET,
  database,
  trustedOrigins: buildTrustedOidcOrigins(serverRedirectUrls),
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
      trustedClients: buildTrustedOidcClients(
        process.env.WADDLE_SERVER_OIDC_CLIENT_SECRET ?? "replace-me",
        serverRedirectUrls,
      ),
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
