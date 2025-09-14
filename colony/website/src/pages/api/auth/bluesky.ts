import type { APIRoute } from 'astro';
import { createBlueskyOAuthClient } from '../../../lib/auth/bluesky-oauth';
import type { BlueskyJWK } from '../../../lib/auth/keys';
import { resolveHandle } from '../../../lib/auth/handle-resolver';

export const POST: APIRoute = async ({ request, locals, url, cookies }) => {
  try {
    let body;
    try {
      body = await request.json();
    } catch (parseError) {
      console.error('Failed to parse request body:', parseError);
      return new Response(JSON.stringify({ error: 'Invalid request body' }), {
        status: 400,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    
    const { handle } = body;
    
    if (!handle || typeof handle !== 'string') {
      return new Response(JSON.stringify({ error: 'Handle is required' }), {
        status: 400,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    
    let privateKeyStr;
    try {
      privateKeyStr = await locals.runtime.env.ATPROTO_PRIVATE_KEY.get();
    } catch (error) {
      console.error('Failed to retrieve private key:', error);
      return new Response(JSON.stringify({ error: 'OAuth configuration error' }), {
        status: 500,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    
    if (!privateKeyStr) {
      return new Response(JSON.stringify({ error: 'OAuth not configured' }), {
        status: 500,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    
    const privateKey: BlueskyJWK = JSON.parse(privateKeyStr);
    const baseUrl = url.origin;
    
    console.log('Initializing OAuth for handle:', handle);
    console.log('Base URL:', baseUrl);
    
    const client = await createBlueskyOAuthClient({
      baseUrl,
      privateKey,
      db: locals.runtime.env.DB,
    });
    
    // Generate state for CSRF protection
    const state = crypto.randomUUID();
    
    // Store state in cookie for verification
    cookies.set('oauth_state', state, {
      httpOnly: true,
      secure: url.protocol === 'https:',
      sameSite: 'lax',
      maxAge: 60 * 10, // 10 minutes
      path: '/',
    });
    
    console.log('Attempting to authorize handle:', handle);
    
    // The OAuth client needs the handle, not the DID
    // But let's verify the handle resolves first
    try {
      console.log('Verifying handle resolution...');
      const did = await resolveHandle(handle);
      console.log('Handle verified, DID:', did);
    } catch (resolveError) {
      console.error('Handle resolution failed:', resolveError);
      return new Response(JSON.stringify({ 
        error: `Could not verify handle "${handle}". Please ensure your handle is correct and accessible.`
      }), {
        status: 400,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    
    try {
      // Initialize OAuth flow with the original handle (not the DID)
      console.log('Initializing OAuth with handle:', handle);
      const authUrl = await client.authorize(handle, { state });
      
      console.log('Successfully got auth URL:', authUrl);
      
      return new Response(JSON.stringify({ authUrl }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    } catch (innerError) {
      console.error('Failed during authorize:', innerError);
      throw innerError;
    }
  } catch (error) {
    console.error('OAuth initiation error:', error);
    console.error('Error stack:', error instanceof Error ? error.stack : 'No stack');
    const errorMessage = error instanceof Error ? error.message : 'Failed to initiate OAuth';
    
    // Check if it's a handle resolution error
    if (errorMessage.includes('Failed to resolve identity')) {
      return new Response(JSON.stringify({ 
        error: `Authentication failed. The Bluesky handle could not be resolved or authorized. Please check your handle and try again.`
      }), {
        status: 400,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    
    return new Response(JSON.stringify({ error: errorMessage }), {
      status: 500,
      headers: { 'Content-Type': 'application/json' },
    });
  }
};