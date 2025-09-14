import type { APIRoute } from 'astro';
import { createWorkOSClient, createSession } from '../../../lib/auth/workos';

export const GET: APIRoute = async ({ locals, url, redirect, cookies }) => {
  const code = url.searchParams.get('code');
  const state = url.searchParams.get('state');
  
  if (!code || !state) {
    return new Response('Missing code or state', { status: 400 });
  }
  
  const { returnUrl, app } = JSON.parse(state);
  const workos = createWorkOSClient(locals.runtime.env.WORKOS_API_KEY);
  
  try {
    const { user } = await workos.userManagement.authenticateWithCode({
      code,
      clientId: locals.runtime.env.WORKOS_CLIENT_ID,
    });
    
    const sessionId = await createSession(locals.runtime.env.AUTH_DB, {
      userId: user.id,
      email: user.email,
      organizationId: user.organizationId,
      expiresAt: Date.now() + (86400 * 7 * 1000), // 7 days
    });
    
    cookies.set('colony_session', sessionId, {
      httpOnly: true,
      secure: true,
      sameSite: 'lax',
      maxAge: 86400 * 7,
      path: '/',
    });
    
    const appDomains: Record<string, string> = {
      waddle: 'https://waddle.social',
      huddle: 'https://huddle.waddle.social',
    };
    
    const appDomain = appDomains[app] || appDomains.waddle;
    return redirect(`${appDomain}${returnUrl}`);
  } catch (error) {
    console.error('Auth error:', error);
    return new Response('Authentication failed', { status: 500 });
  }
};