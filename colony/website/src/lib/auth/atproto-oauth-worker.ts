/**
 * Simplified ATProto OAuth implementation for Cloudflare Workers
 * This avoids Node.js specific dependencies
 */

import { SignJWT, importJWK, generateKeyPair, exportJWK } from 'jose';
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
      scope: 'atproto transition:email',
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
      jti: crypto.randomUUID(), // Add unique JWT ID to prevent replay attacks
    })
      .setProtectedHeader({
        alg: 'ES256',
        kid: this.config.privateKey.kid,
      })
      .sign(privateKey);

    return jwt;
  }

  async createDPoPProof(
    method: string,
    url: string,
    nonce?: string,
    dpopKeyPair?: { publicKey: any, privateKey: any },
    accessToken?: string
  ): Promise<{ proof: string; publicKey: any; keyPair: any }> {
    // Generate a new key pair for DPoP if not provided
    let keyPair = dpopKeyPair;
    if (!keyPair) {
      const generated = await generateKeyPair('ES256');
      keyPair = {
        publicKey: generated.publicKey,
        privateKey: generated.privateKey
      };
    }

    // Export the public key to JWK format
    const publicJwk = await exportJWK(keyPair.publicKey);

    // Ensure the JWK has all required fields for ATProto
    if (!publicJwk.kty) publicJwk.kty = 'EC';
    if (!publicJwk.crv) publicJwk.crv = 'P-256';
    if (!publicJwk.use) publicJwk.use = 'sig';
    if (!publicJwk.alg) publicJwk.alg = 'ES256';

    // Remove private key components if present
    delete publicJwk.d;
    delete publicJwk.p;
    delete publicJwk.q;
    delete publicJwk.dp;
    delete publicJwk.dq;
    delete publicJwk.qi;

    // Build the payload
    const payload: any = {
      jti: crypto.randomUUID(),
      htm: method.toUpperCase(),
      htu: url,
      iat: Math.floor(Date.now() / 1000),
      exp: Math.floor(Date.now() / 1000) + 60, // Expires in 60 seconds
    };

    // Add nonce if provided (required for subsequent requests after first token request)
    if (nonce) {
      payload.nonce = nonce;
    }

    // Add access token hash (ath) for resource server requests
    if (accessToken) {
      // Calculate S256 hash of the access token (same as PKCE challenge)
      const encoder = new TextEncoder();
      const data = encoder.encode(accessToken);
      const hashBuffer = await crypto.subtle.digest('SHA-256', data);
      const hashArray = new Uint8Array(hashBuffer);
      // Convert to base64url
      const ath = btoa(String.fromCharCode(...hashArray))
        .replace(/\+/g, '-')
        .replace(/\//g, '_')
        .replace(/=/g, '');
      payload.ath = ath;
    }

    // Create the DPoP proof JWT
    const dpopProof = await new SignJWT(payload)
      .setProtectedHeader({
        typ: 'dpop+jwt',
        alg: 'ES256',
        jwk: publicJwk,
      })
      .sign(keyPair.privateKey);

    return {
      proof: dpopProof,
      publicKey: publicJwk,
      keyPair // Return the key pair for reuse
    };
  }
}