import { drizzleAdapter } from "@better-auth/drizzle-adapter";
import { betterAuth } from "better-auth";
import { env } from "cloudflare:workers";

import { db } from "../db";

const database = drizzleAdapter(db, {
  provider: "sqlite",
}) as unknown as Parameters<typeof betterAuth>[0]["database"];

export const auth = betterAuth({
  baseURL: env.BETTER_AUTH_URL,
  secret: env.BETTER_AUTH_SECRET,
  database,
  socialProviders: {
    github: {
      clientId: env.GITHUB_CLIENT_ID,
      clientSecret: env.GITHUB_CLIENT_SECRET,
      scope: ["read:user", "user:email"],
    },
  },
});
