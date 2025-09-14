/**
 * Simplified ATProto OAuth implementation for Cloudflare Workers
 * This avoids Node.js specific dependencies
 */

import { SignJWT, importJWK } from 'jose';
import type { BlueskyJWK } from './keys';
import { resolveHandle } from './handle-resolver';

interface OAuthConfig {
  clientId: string;
  redirectUri: string;
  privateKey: BlueskyJWK;
}

export class WorkerOAuthClient {
  constructor(private config: OAuthConfig) {}

  async createAuthorizationUrl(handle: string, state: string): Promise<string> {
    // First resolve the handle to get the DID and PDS
    const did = await resolveHandle(handle);
    
    // For now, we'll use the default Bluesky PDS
    // In production, you'd want to discover the correct PDS for the user
    const pdsUrl = 'https://bsky.social';
    
    // Build the authorization URL
    const params = new URLSearchParams({
      response_type: 'code',
      client_id: this.config.clientId,
      redirect_uri: this.config.redirectUri,
      state: state,
      scope: 'atproto transition:generic',
      login_hint: handle,
    });

    // The authorization endpoint for Bluesky
    const authUrl = `${pdsUrl}/oauth/authorize?${params.toString()}`;
    
    console.log('Created auth URL:', authUrl);
    return authUrl;
  }

  async createClientAssertion(): Promise<string> {
    const privateKey = await importJWK(this.config.privateKey, 'ES256');
    
    const jwt = await new SignJWT({
      iss: this.config.clientId,
      sub: this.config.clientId,
      aud: 'https://bsky.social',
      iat: Math.floor(Date.now() / 1000),
      exp: Math.floor(Date.now() / 1000) + 60,
    })
      .setProtectedHeader({ 
        alg: 'ES256',
        kid: this.config.privateKey.kid,
      })
      .sign(privateKey);
    
    return jwt;
  }
}