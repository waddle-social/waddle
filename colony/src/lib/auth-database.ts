import { drizzleAdapter } from "@better-auth/drizzle-adapter";
import { BetterAuthError } from "better-auth";

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

type AdapterCall = {
  model: string;
};

type AdapterShape = {
  count: (input: AdapterCall) => Promise<number>;
  create: (input: AdapterCall & Record<string, unknown>) => Promise<unknown>;
  delete: (input: AdapterCall & Record<string, unknown>) => Promise<unknown>;
  deleteMany: (input: AdapterCall & Record<string, unknown>) => Promise<number>;
  findMany: (input: AdapterCall & Record<string, unknown>) => Promise<unknown[]>;
  findOne: (input: AdapterCall & Record<string, unknown>) => Promise<unknown>;
  update: (input: AdapterCall & Record<string, unknown>) => Promise<unknown>;
  updateMany: (input: AdapterCall & Record<string, unknown>) => Promise<unknown>;
  options: unknown;
};

const blockedModel = "oauthApplication";
const blockedMessage = "Database-backed OIDC clients are disabled.";

const baseDatabase = drizzleAdapter(db, {
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
}) as unknown as (options: unknown) => AdapterShape;

function assertAllowedModel(input: AdapterCall) {
  if (input.model === blockedModel) {
    throw new BetterAuthError(blockedMessage);
  }
}

export const database = ((options: unknown) => {
  const adapter = baseDatabase(options);

  return {
    ...adapter,
    async count(input: AdapterCall) {
      if (input.model === blockedModel) {
        return 0;
      }

      return adapter.count(input);
    },
    async create(input: AdapterCall & Record<string, unknown>) {
      assertAllowedModel(input);
      return adapter.create(input);
    },
    async delete(input: AdapterCall & Record<string, unknown>) {
      assertAllowedModel(input);
      return adapter.delete(input);
    },
    async deleteMany(input: AdapterCall & Record<string, unknown>) {
      if (input.model === blockedModel) {
        return 0;
      }

      return adapter.deleteMany(input);
    },
    async findMany(input: AdapterCall & Record<string, unknown>) {
      if (input.model === blockedModel) {
        return [];
      }

      return adapter.findMany(input);
    },
    async findOne(input: AdapterCall & Record<string, unknown>) {
      if (input.model === blockedModel) {
        return null;
      }

      return adapter.findOne(input);
    },
    async update(input: AdapterCall & Record<string, unknown>) {
      assertAllowedModel(input);
      return adapter.update(input);
    },
    async updateMany(input: AdapterCall & Record<string, unknown>) {
      assertAllowedModel(input);
      return adapter.updateMany(input);
    },
  };
}) as unknown as Parameters<typeof import("better-auth").betterAuth>[0]["database"];
