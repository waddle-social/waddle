import {
  oauthProviderAuthServerMetadata,
  oauthProviderOpenIdConfigMetadata,
} from "@better-auth/oauth-provider";

import { getAuth } from "./auth";

export async function getOpenIdConfiguration(request: Request) {
  const auth = await getAuth();
  return oauthProviderOpenIdConfigMetadata(auth)(request);
}

export async function getOAuthAuthorizationServerMetadata(request: Request) {
  const auth = await getAuth();
  return oauthProviderAuthServerMetadata(auth)(request);
}
