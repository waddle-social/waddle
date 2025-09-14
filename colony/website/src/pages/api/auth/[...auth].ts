import type { APIRoute } from 'astro';
import { createAuth } from '../../../lib/auth/auth';
import type { BlueskyJWK } from '../../../lib/auth/keys';

export const ALL: APIRoute = async (context) => {
  const { request, locals, url } = context;
  
  // Get the private key from Secrets Store
  let privateKeyStr;
  try {
    privateKeyStr = await locals.runtime.env.ATPROTO_PRIVATE_KEY.get();
  } catch (error) {
    console.error('Failed to retrieve private key:', error);
    return new Response('Authentication service unavailable', { status: 500 });
  }
  
  if (!privateKeyStr) {
    return new Response('Authentication not configured', { status: 500 });
  }
  
  const auth = createAuth(
    locals.runtime.env.DB,
    privateKeyStr,
    url.origin
  );
  
  // BetterAuth handles all the auth routes
  return auth.handler(request);
};