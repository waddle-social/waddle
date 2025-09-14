import { betterAuth } from 'better-auth';
import { drizzleAdapter } from 'better-auth/adapters/drizzle';
import { drizzle } from 'drizzle-orm/d1';
import * as schema from '../db/schema';
import type { D1Database } from '@cloudflare/workers-types';

export function createAuth(db: D1Database, privateKey: string, baseUrl: string) {
  const drizzleDb = drizzle(db, { schema });
  
  return betterAuth({
    database: drizzleAdapter(drizzleDb, {
      provider: 'sqlite',
    }),
    baseURL: baseUrl,
    socialProviders: {
      bluesky: {
        clientId: `${baseUrl}/client-metadata.json`,
        clientSecret: privateKey, // The private key acts as the secret for JWT signing
        authorizationUrl: 'https://bsky.social/oauth/authorize',
        tokenUrl: 'https://bsky.social/oauth/token',
        userInfoUrl: 'https://bsky.social/xrpc/com.atproto.identity.getRecommendedDidCredentials',
        scope: ['atproto', 'transition:generic'],
        // Custom implementation for Bluesky's unique OAuth flow
        async getUserInfo(tokens: any) {
          // This would need to be implemented based on Bluesky's API
          return {
            id: tokens.sub,
            email: null,
            name: tokens.handle,
            image: tokens.avatar,
          };
        },
      },
    },
    session: {
      expiresIn: 60 * 60 * 24 * 7, // 7 days
      updateAge: 60 * 60 * 24, // 1 day
    },
  });
}