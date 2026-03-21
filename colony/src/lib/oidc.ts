import type { Client } from "better-auth/plugins/oidc-provider";

export const CHAT_OIDC_CLIENT_ID = "waddle-chat";

export const trustedOidcClients: Client[] = [
  {
    clientId: CHAT_OIDC_CLIENT_ID,
    clientSecret: undefined,
    type: "public",
    name: "Waddle Chat",
    icon: undefined,
    metadata: {
      firstParty: true,
      product: "chat",
    },
    disabled: false,
    redirectUrls: [
      "http://localhost:4321/api/auth/oauth2/callback/colony",
      "https://chat.waddle.social/api/auth/oauth2/callback/colony",
    ],
    skipConsent: true,
  },
];
