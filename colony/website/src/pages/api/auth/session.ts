import type { APIRoute } from 'astro';
import { getSession } from '../../../lib/auth/workos';

export const GET: APIRoute = async ({ locals, cookies }) => {
  const sessionId = cookies.get('colony_session')?.value;
  
  if (!sessionId) {
    return new Response(JSON.stringify({ authenticated: false }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    });
  }
  
  const session = await getSession(locals.runtime.env.AUTH_DB, sessionId);
  
  if (!session) {
    cookies.delete('colony_session', { path: '/' });
    return new Response(JSON.stringify({ authenticated: false }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    });
  }
  
  return new Response(JSON.stringify({
    authenticated: true,
    user: {
      id: session.userId,
      email: session.email,
      organizationId: session.organizationId,
    },
  }), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  });
};