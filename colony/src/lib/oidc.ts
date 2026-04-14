import type { Client } from "better-auth/plugins/oidc-provider";

export const SERVER_OIDC_CLIENT_ID = "waddle-server";
const SERVER_OIDC_REDIRECT_URLS_ENV = "WADDLE_SERVER_OIDC_REDIRECT_URLS";

const DEFAULT_SERVER_OIDC_REDIRECT_URLS = [
  "http://localhost:3000/api/auth/callback",
];
const SUPPORTED_REDIRECT_PROTOCOLS = new Set(["http:", "https:"]);

function normalizeRedirectUrl(rawValue: string): string {
  if (!URL.canParse(rawValue)) {
    throw new Error(`Invalid URL in ${SERVER_OIDC_REDIRECT_URLS_ENV}: ${rawValue}`);
  }

  const parsed = new URL(rawValue);
  if (!SUPPORTED_REDIRECT_PROTOCOLS.has(parsed.protocol)) {
    throw new Error(
      `Unsupported protocol in ${SERVER_OIDC_REDIRECT_URLS_ENV}: ${rawValue}`,
    );
  }

  return parsed.toString();
}

export function resolveServerOidcRedirectUrls(
  envValue: string | undefined,
): string[] {
  if (!envValue || envValue.trim().length === 0) {
    return [...DEFAULT_SERVER_OIDC_REDIRECT_URLS];
  }

  const rawRedirectUrls = envValue
    .split(",")
    .map((value) => value.trim())
    .filter((value) => value.length > 0);

  if (rawRedirectUrls.length === 0) {
    throw new Error(
      `${SERVER_OIDC_REDIRECT_URLS_ENV} is set but does not contain any URLs`,
    );
  }

  return [...new Set(rawRedirectUrls.map(normalizeRedirectUrl))];
}

function buildOidcClientDefinitions(serverRedirectUrls: string[]) {
  return [
    {
      clientId: SERVER_OIDC_CLIENT_ID,
      type: "web" as const,
      name: "Waddle Server",
      metadata: {
        firstParty: true,
        product: "server",
      },
      redirectUrls: serverRedirectUrls,
      skipConsent: true,
    },
  ];
}

export function buildTrustedOidcClients(
  serverClientSecret: string,
  serverRedirectUrls: string[],
): Client[] {
  return buildOidcClientDefinitions(serverRedirectUrls).map((client) => ({
    ...client,
    clientSecret: serverClientSecret,
    icon: undefined,
    disabled: false,
  }));
}

export function buildTrustedOidcOrigins(serverRedirectUrls: string[]): string[] {
  return [
    ...new Set(
      buildOidcClientDefinitions(serverRedirectUrls).flatMap((client) =>
        client.redirectUrls.map((redirectUrl) => new URL(redirectUrl).origin),
      ),
    ),
  ];
}
