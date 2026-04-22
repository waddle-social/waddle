import { drizzleAdapter } from "@better-auth/drizzle-adapter";
import type { betterAuth } from "better-auth";

import { db } from "../db";
import {
  account,
  jwks,
  oauthAccessToken,
  oauthClient,
  oauthConsent,
  oauthRefreshToken,
  session,
  user,
  verification,
} from "../db/schema";

export const database = drizzleAdapter(db, {
  provider: "sqlite",
  schema: {
    user,
    session,
    account,
    verification,
    jwks,
    oauthClient,
    oauthRefreshToken,
    oauthAccessToken,
    oauthConsent,
  },
}) as unknown as Parameters<typeof betterAuth>[0]["database"];
