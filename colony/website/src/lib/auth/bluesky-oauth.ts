import { NodeOAuthClient, JoseKey } from '@atproto/oauth-client-node';
import type { NodeSavedState, NodeSavedSession } from '@atproto/oauth-client-node';
import { resolveHandle } from './handle-resolver';
import { drizzle } from 'drizzle-orm/d1';
import { eq } from 'drizzle-orm';
import * as schema from '../db/schema';
import type { D1Database } from '@cloudflare/workers-types';
import type { BlueskyJWK } from './keys';

interface BlueskyOAuthConfig {
  baseUrl: string;
  privateKey: BlueskyJWK;
  db: D1Database;
}

export async function createBlueskyOAuthClient(config: BlueskyOAuthConfig) {
  const { baseUrl, privateKey, db } = config;
  const drizzleDb = drizzle(db, { schema });
  
  console.log('Creating Bluesky OAuth client with base URL:', baseUrl);
  
  const clientMetadata = {
    client_id: `${baseUrl}/client-metadata.json`,
    application_type: 'web' as const,
    client_name: 'Waddle Colony',
    redirect_uris: [`${baseUrl}/api/auth/bluesky/callback`],
    grant_types: ['authorization_code', 'refresh_token'],
    response_types: ['code'],
    scope: 'atproto transition:generic',
    dpop_bound_access_tokens: true,
    token_endpoint_auth_method: 'private_key_jwt' as const,
    jwks_uri: `${baseUrl}/jwks.json`,
    token_endpoint_auth_signing_alg: 'ES256' as const,
  };

  const keyset = await Promise.all([
    JoseKey.fromImportable(JSON.stringify(privateKey)),
  ]);

  return new NodeOAuthClient({
    clientMetadata,
    keyset,
    stateStore: {
      async set(key: string, internalState: NodeSavedState): Promise<void> {
        // Store in D1 using a temporary table or session storage
        // For simplicity, we'll use the verifications table temporarily
        await drizzleDb.insert(schema.verifications).values({
          id: key,
          identifier: 'oauth_state',
          value: JSON.stringify(internalState),
          expiresAt: new Date(Date.now() + 600000), // 10 minutes
          createdAt: new Date(),
          updatedAt: new Date(),
        });
      },
      async get(key: string): Promise<NodeSavedState | undefined> {
        const result = await drizzleDb
          .select()
          .from(schema.verifications)
          .where(eq(schema.verifications.id, key))
          .limit(1);
        
        if (result.length === 0) return undefined;
        
        const state = result[0];
        if (state.expiresAt < new Date()) {
          await drizzleDb.delete(schema.verifications).where(eq(schema.verifications.id, key));
          return undefined;
        }
        
        return JSON.parse(state.value) as NodeSavedState;
      },
      async del(key: string): Promise<void> {
        await drizzleDb.delete(schema.verifications).where(eq(schema.verifications.id, key));
      },
    },
    sessionStore: {
      async set(sub: string, session: NodeSavedSession): Promise<void> {
        // Check if account exists
        const existingAccount = await drizzleDb
          .select()
          .from(schema.accounts)
          .where(eq(schema.accounts.accountId, sub))
          .limit(1);
        
        const sessionData = {
          accountId: sub,
          providerId: 'bluesky',
          accessToken: session.tokenSet.access_token,
          refreshToken: session.tokenSet.refresh_token,
          accessTokenExpiresAt: session.tokenSet.expires_at ? new Date(session.tokenSet.expires_at * 1000) : null,
          scope: session.tokenSet.scope,
          dpopState: JSON.stringify(session.dpopState),
          sessionState: JSON.stringify(session),
        };
        
        if (existingAccount.length > 0) {
          // Update existing account
          await drizzleDb
            .update(schema.accounts)
            .set({
              ...sessionData,
              updatedAt: new Date(),
            })
            .where(eq(schema.accounts.accountId, sub));
        } else {
          // Create new account and user
          const userId = crypto.randomUUID();
          
          // Create user first
          await drizzleDb.insert(schema.users).values({
            id: userId,
            did: sub,
            createdAt: new Date(),
            updatedAt: new Date(),
          });
          
          // Create account
          await drizzleDb.insert(schema.accounts).values({
            id: crypto.randomUUID(),
            userId,
            ...sessionData,
            createdAt: new Date(),
            updatedAt: new Date(),
          });
        }
      },
      async get(sub: string): Promise<NodeSavedSession | undefined> {
        const result = await drizzleDb
          .select()
          .from(schema.accounts)
          .where(eq(schema.accounts.accountId, sub))
          .limit(1);
        
        if (result.length === 0) return undefined;
        
        const account = result[0];
        if (!account.sessionState) return undefined;
        
        return JSON.parse(account.sessionState) as NodeSavedSession;
      },
      async del(sub: string): Promise<void> {
        await drizzleDb
          .delete(schema.accounts)
          .where(eq(schema.accounts.accountId, sub));
      },
    },
  });
}

export async function getBlueskyUserInfo(client: NodeOAuthClient, did: string) {
  const session = await client.restore(did);
  if (!session) return null;
  
  // Get user profile from atproto
  const response = await session.request(
    'GET',
    'com.atproto.repo.describeRepo',
    { params: { repo: did } }
  );
  
  return response.data;
}