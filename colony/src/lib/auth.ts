import { betterAuth } from "better-auth";
import { jwt, oidcProvider } from "better-auth/plugins";
import { env } from "cloudflare:workers";

import { db } from "../db";
import { oauthApplication } from "../db/schema";
import { database } from "./auth-database";
import {
  buildTrustedOidcClients,
  buildTrustedOidcOrigins,
  resolveServerOidcRedirectUrls,
  SERVER_OIDC_CLIENT_ID,
} from "./oidc";

const SERVER_OIDC_METADATA = JSON.stringify({
  firstParty: true,
  product: "server",
});

async function syncServerOidcApplication(serverRedirectUrls: string[]) {
  const now = new Date();
  await db
    .insert(oauthApplication)
    .values({
      id: SERVER_OIDC_CLIENT_ID,
      name: "Waddle Server",
      metadata: SERVER_OIDC_METADATA,
      clientId: SERVER_OIDC_CLIENT_ID,
      redirectUrls: JSON.stringify(serverRedirectUrls),
      type: "web",
      disabled: false,
      createdAt: now,
      updatedAt: now,
    })
    .onConflictDoUpdate({
      target: oauthApplication.clientId,
      set: {
        name: "Waddle Server",
        metadata: SERVER_OIDC_METADATA,
        redirectUrls: JSON.stringify(serverRedirectUrls),
        type: "web",
        disabled: false,
        updatedAt: now,
      },
    });
}

function createAuth(
  secret: string,
  githubClientSecret: string,
  serverOidcClientSecret: string,
  serverRedirectUrls: string[],
) {
  return betterAuth({
    baseURL: env.BETTER_AUTH_URL,
    secret,
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
        trustedClients: buildTrustedOidcClients(
          serverOidcClientSecret,
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
}

let authPromise: Promise<ReturnType<typeof createAuth>> | undefined;

export function getAuth() {
  authPromise ??= Promise.all([
    env.BETTER_AUTH_SECRET.get(),
    env.GITHUB_CLIENT_SECRET.get(),
    env.WADDLE_SERVER_OIDC_CLIENT_SECRET.get(),
  ]).then(async ([secret, githubClientSecret, serverOidcClientSecret]) => {
    const serverRedirectUrls = resolveServerOidcRedirectUrls(
      env.WADDLE_SERVER_OIDC_REDIRECT_URLS,
    );
    await syncServerOidcApplication(serverRedirectUrls);
    return createAuth(
      secret,
      githubClientSecret,
      serverOidcClientSecret,
      serverRedirectUrls,
    );
  });

  return authPromise;
}
