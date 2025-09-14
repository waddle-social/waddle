import type { APIRoute } from 'astro';
import { getPublicKeyFromPrivate, type BlueskyJWK } from '../lib/auth/keys';

export const GET: APIRoute = async ({ locals }) => {
  // Get the private key from Secrets Store
  let privateKeyStr;
  try {
    privateKeyStr = await locals.runtime.env.ATPROTO_PRIVATE_KEY.get();
  } catch (error) {
    console.error('Failed to retrieve private key for JWKS:', error);
    return new Response(JSON.stringify({ error: 'Keys not available' }), {
      status: 500,
      headers: {
        'Content-Type': 'application/json',
      },
    });
  }
  
  if (!privateKeyStr) {
    return new Response(JSON.stringify({ error: 'Keys not configured' }), {
      status: 500,
      headers: {
        'Content-Type': 'application/json',
      },
    });
  }

  try {
    const privateKey: BlueskyJWK = JSON.parse(privateKeyStr);
    const publicKey = getPublicKeyFromPrivate(privateKey);
    
    // JWKS format requires a keys array
    const jwks = {
      keys: [publicKey]
    };
    
    return new Response(JSON.stringify(jwks), {
      status: 200,
      headers: {
        'Content-Type': 'application/json',
        'Cache-Control': 'public, max-age=3600', // Cache for 1 hour
      },
    });
  } catch (error) {
    console.error('Error serving JWKS:', error);
    return new Response(JSON.stringify({ error: 'Invalid key configuration' }), {
      status: 500,
      headers: {
        'Content-Type': 'application/json',
      },
    });
  }
};