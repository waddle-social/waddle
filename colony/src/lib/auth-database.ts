import { drizzleAdapter } from "@better-auth/drizzle-adapter";
import type { betterAuth } from "better-auth";

import { db } from "../db";
import {
  account,
  jwks,
  oauthAccessToken,
  oauthApplication,
  oauthConsent,
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
    oauthApplication,
    oauthAccessToken,
    oauthConsent,
  },
}) as unknown as Parameters<typeof betterAuth>[0]["database"];
