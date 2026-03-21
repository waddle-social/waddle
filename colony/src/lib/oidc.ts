import type { Client } from "better-auth/plugins/oidc-provider";

export const SERVER_OIDC_CLIENT_ID = "waddle-server";

const oidcClientDefinitions = [
  {
    clientId: SERVER_OIDC_CLIENT_ID,
    type: "web" as const,
    name: "Waddle Server",
    metadata: {
      firstParty: true,
      product: "server",
    },
    redirectUrls: [
      "http://localhost:3000/api/auth/callback",
      "https://server.waddle.social/api/auth/callback",
    ],
    skipConsent: true,
  },
];

export function buildTrustedOidcClients(serverClientSecret: string): Client[] {
  return oidcClientDefinitions.map((client) => ({
    ...client,
    clientSecret: serverClientSecret,
    icon: undefined,
    disabled: false,
  }));
}

export const trustedOidcOrigins = [
  ...new Set(
    oidcClientDefinitions.flatMap((client) =>
      client.redirectUrls.map((redirectUrl) => new URL(redirectUrl).origin),
    ),
  ),
];
