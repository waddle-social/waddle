import { betterAuth } from "better-auth";
import { jwt, oidcProvider } from "better-auth/plugins";

import { database } from "./auth-database";
import { buildTrustedOidcClients, trustedOidcOrigins } from "./oidc";

export const auth = betterAuth({
  baseURL: process.env.BETTER_AUTH_URL,
  secret: process.env.BETTER_AUTH_SECRET,
  database,
  disabledPaths: ["/token", "/oauth2/register"],
  trustedOrigins: trustedOidcOrigins,
  socialProviders: {
    github: {
      clientId: process.env.GITHUB_CLIENT_ID,
      clientSecret: process.env.GITHUB_CLIENT_SECRET,
      scope: ["read:user", "user:email"],
    },
  },
  plugins: [
    jwt({
      disableSettingJwtHeader: true,
    }),
    oidcProvider({
      loginPage: "/",
      trustedClients: buildTrustedOidcClients(
        process.env.WADDLE_SERVER_OIDC_CLIENT_SECRET ?? "replace-me",
      ),
      useJWTPlugin: true,
    }),
  ],
});
