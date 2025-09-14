import { generateKeyPair, exportJWK, importJWK } from 'jose';
import type { JWK } from 'jose';

export interface BlueskyJWK extends JWK {
  kty: 'EC';
  use: 'sig';
  alg: 'ES256';
  kid: string;
  crv: 'P-256';
  x: string;
  y: string;
  d?: string; // Private key component (only in private key)
}

export async function generateES256KeyPair(): Promise<{
  privateKey: BlueskyJWK;
  publicKey: BlueskyJWK;
}> {
  const { publicKey, privateKey } = await generateKeyPair('ES256', {
    extractable: true,
  });

  const kid = crypto.randomUUID();
  
  const privateJWK = await exportJWK(privateKey) as BlueskyJWK;
  const publicJWK = await exportJWK(publicKey) as BlueskyJWK;

  // Add required fields for Bluesky OAuth
  privateJWK.kid = kid;
  privateJWK.use = 'sig';
  privateJWK.alg = 'ES256';
  
  publicJWK.kid = kid;
  publicJWK.use = 'sig';
  publicJWK.alg = 'ES256';
  
  // Remove private key from public JWK (safety check)
  delete publicJWK.d;

  return { privateKey: privateJWK, publicKey: publicJWK };
}

export function getPublicKeyFromPrivate(privateKey: BlueskyJWK): BlueskyJWK {
  const { d, ...publicKey } = privateKey;
  return publicKey as BlueskyJWK;
}

export async function importPrivateKey(jwk: BlueskyJWK) {
  return await importJWK(jwk, 'ES256');
}