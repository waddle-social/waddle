import type { APIRoute } from 'astro';

export const POST: APIRoute = async ({ request, url, cookies }) => {
  try {
    const body = await request.json();
    const { handle } = body;
    
    if (!handle || typeof handle !== 'string') {
      return new Response(JSON.stringify({ error: 'Handle is required' }), {
        status: 400,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    
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
    
    // Build OAuth authorization URL directly
    const params = new URLSearchParams({
      response_type: 'code',
      client_id: `${url.origin}/client-metadata.json`,
      redirect_uri: `${url.origin}/api/auth/bluesky/callback`,
      state: state,
      scope: 'atproto transition:generic',
      login_hint: handle,
    });
    
    // Use Bluesky's OAuth endpoint
    const authUrl = `https://bsky.social/oauth/authorize?${params.toString()}`;
    
    return new Response(JSON.stringify({ authUrl }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    });
  } catch (error) {
    console.error('OAuth error:', error);
    return new Response(JSON.stringify({ 
      error: 'Failed to initiate authentication' 
    }), {
      status: 500,
      headers: { 'Content-Type': 'application/json' },
    });
  }
};