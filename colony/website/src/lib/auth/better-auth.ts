import { betterAuth } from 'better-auth';
import { drizzleAdapter } from 'better-auth/adapters/drizzle';
import { drizzle } from 'drizzle-orm/d1';
import * as schema from '../db/schema';
import type { D1Database } from '@cloudflare/workers-types';

export function createAuth(db: D1Database, env: any) {
  const drizzleDb = drizzle(db, { schema });
  
  return betterAuth({
    database: drizzleAdapter(drizzleDb, {
      provider: 'sqlite',
    }),
    emailAndPassword: {
      enabled: false, // We only use Bluesky OAuth
    },
    socialProviders: {
      custom: [
        {
          id: 'bluesky',
          name: 'Bluesky',
          createAuthorizationURL: async ({ state }) => {
            // This will be implemented with the Bluesky OAuth client
            throw new Error('Use BlueskyOAuthClient for authorization');
          },
          validateAuthorizationCode: async ({ code }) => {
            // This will be implemented with the Bluesky OAuth client
            throw new Error('Use BlueskyOAuthClient for validation');
          },
        },
      ],
    },
    session: {
      expiresIn: 60 * 60 * 24 * 7, // 7 days
      updateAge: 60 * 60 * 24, // 1 day
      cookieCache: {
        enabled: true,
        maxAge: 60 * 5, // 5 minutes
      },
    },
    trustedOrigins: [
      'https://auth.waddle.social',
      'http://localhost:4321', // For local development
    ],
  });
}

export type Auth = ReturnType<typeof createAuth>;